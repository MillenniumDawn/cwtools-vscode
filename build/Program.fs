open System
open System.IO
open Fake.Core
open Fake.DotNet
open Fake.IO
open Fake.IO.FileSystemOperators
open Fake.IO.Globbing.Operators
open Fake.Core.TargetOperators
open Fake.Tools.Git
open Fake.Api
open System.Text.Json

// --------------------------------------------------------------------------------------
// Configuration
// --------------------------------------------------------------------------------------


// Git configuration (used for publishing documentation in gh-pages branch)
// The profile where the project is posted
let gitOwner = "MillenniumDawn"
let gitHome = "https://github.com/" + gitOwner

// The name of the project on GitHub
let gitName = "cwtools-vscode"


// Read additional information from the release notes document
let releaseNotesData = File.ReadAllLines "CHANGELOG.md" |> ReleaseNotes.parseAll

let release = List.head releaseNotesData

let githubToken = Environment.environVarOrNone "GITHUB_TOKEN"

// On a tag push CI sets TAG_RELEASE=true and the version comes from the tag.
// Manual/local runs fall back to the top CHANGELOG.md entry and create the tag.
let isTagRelease = Environment.environVarAsBoolOrDefault "TAG_RELEASE" false

let releaseTag =
    match isTagRelease, Environment.environVarOrNone "GITHUB_REF_NAME" with
    | true, Some t when not (String.IsNullOrWhiteSpace t) -> t
    | _ -> release.NugetVersion

// package.json wants a bare semver, so drop any leading "v".
let releaseVersion = releaseTag.TrimStart('v')
let isPreRelease = releaseVersion.Contains "-"

// Notes for the GitHub release: the CHANGELOG entry matching the released
// version, falling back to the top entry when there's no exact match.
let releaseNotes =
    releaseNotesData
    |> List.tryFind (fun r -> $"%O{r.NugetVersion}" = releaseVersion)
    |> Option.defaultValue release
    |> fun r -> r.Notes

// open Fake.BuildServer
let platformShortCode =
    match Environment.isWindows, Environment.isMacOS, Environment.isLinux with
    | false, false, true -> "linux-x64"
    | false, true, false -> "osx-x64"
    | _ -> "win-x64"

// BuildServer.install [ GitLab.Installer ]

let run cmd args dir =
    let parms =
        { ExecParams.Empty with
            Program = cmd
            WorkingDir = dir
            CommandLine = args }

    if Process.shellExec parms <> 0 then
        failwithf $"Error while running '%s{cmd}' with args: %s{args}"

let platformTool tool path =
    match Environment.isUnix with
    | true -> tool
    | _ ->
        match ProcessUtils.tryFindFileOnPath path with
        | None -> failwithf $"can't find tool %s{tool} on PATH"
        | Some v -> v

let npxTool = lazy (platformTool "npx" "npx.cmd")
let npmTool = lazy (platformTool "npm" "npm.cmd")

let cwtoolsProjectName = "Main.fsproj"
let cwtoolsProjectPath = "src/Main/Main.fsproj"
let releaseDir = "release"

// The server is the Rust LSP server (cwtools-rs), built in the sibling
// `cwtools` repo. It produces a single standalone binary that the
// extension launches over stdio. Defaults to the sibling layout used in CI;
// set CWTOOLS_RUST_WORKSPACE to build from a checkout somewhere else.
let rustWorkspace =
    match Environment.environVarOrNone "CWTOOLS_RUST_WORKSPACE" with
    | Some p when not (String.IsNullOrWhiteSpace p) -> p
    | _ -> "../cwtools/cwtools-rs"
let rustServerBinName =
    if Environment.isWindows then "cwtools-server.exe" else "cwtools-server"
let rustServerBin = rustWorkspace </> "target/release" </> rustServerBinName
// Deploy to the path the client actually loads first; the old
// bin/server/<platform>/"CWTools Server" path is only a legacy fallback.
let serverOutDir = releaseDir </> "bin/server/cwtools-server"

let buildAndDeployRustServer () =
    run "cargo" "build --release -p cwtools_lsp" rustWorkspace
    if not (File.Exists rustServerBin) then
        failwithf
            $"Rust server binary not found at '%s{rustServerBin}' after build. Check the crate name/target, or point CWTOOLS_RUST_WORKSPACE at the right cwtools-rs checkout (currently '%s{rustWorkspace}')."
    // Clean the server dir so stale F# .NET files (hostfxr, *.dll,
    // *.deps.json) don't linger next to the standalone Rust binary.
    Shell.cleanDir serverOutDir
    let dest = serverOutDir </> rustServerBinName
    System.IO.File.Copy(rustServerBin, dest, true)
    if Environment.isUnix then
        System.IO.File.SetUnixFileMode(
            dest,
            UnixFileMode.UserRead ||| UnixFileMode.UserWrite ||| UnixFileMode.UserExecute
            ||| UnixFileMode.GroupRead ||| UnixFileMode.GroupExecute
            ||| UnixFileMode.OtherRead
        )

// The original F# language server (src/Main/Main.fsproj), deployed
// alongside the Rust one so cwtools.engine can switch between them.
let fsharpServerOutDir = releaseDir </> "bin/server" </> platformShortCode

let buildAndDeployFSharpServer (release: bool) =
    DotNet.build
        (fun b ->
            { b with
                OutputPath = Some fsharpServerOutDir
                Configuration =
                    if release then
                        DotNet.BuildConfiguration.Release
                    else
                        DotNet.BuildConfiguration.Debug
                MSBuildParams =
                    { MSBuild.CliArguments.Create() with
                        DisableInternalBinLog = true } })
        cwtoolsProjectPath

// --------------------------------------------------------------------------------------
// Build the Generator project and run it
// --------------------------------------------------------------------------------------

let buildPackage dir =
    Process.killAllByName "npx"
    // The client is bundled with esbuild, so node_modules is excluded from the
    // vsix (see .vscodeignore). --no-dependencies stops vsce from trying to
    // resolve/include them.
    run npxTool.Value "--yes @vscode/vsce package --no-dependencies" dir

    // ReleaseGitHub reads the vsix from ./temp. The prebuilt path skips Clean,
    // so ensure it exists here rather than relying on Clean to create it.
    Directory.ensure "./temp"
    !! $"%s{dir}/*.vsix" |> Seq.iter (Shell.moveFile "./temp/")

let setPackageJsonField (name: string) (value: string) releaseDir =
    let fileName = $"./%s{releaseDir}/package.json"
    let content = File.readAsString fileName
    let jsonObj = JsonDocument.Parse content
    let node = System.Text.Json.Nodes.JsonObject.Create jsonObj.RootElement
    node[name] <- value
    let opts = JsonSerializerOptions(WriteIndented = true, AllowTrailingCommas = false)
    File.WriteAllText(fileName, node.ToJsonString(opts))

let setVersion releaseDir =
    setPackageJsonField "version" releaseVersion releaseDir

let publishToGallery releaseDir =
    let publish token =
        Process.killAllByName "npx"
        run npxTool.Value $"@vscode/vsce publish --pat %s{token}" releaseDir

    match Environment.environVarOrDefault "vsce-token" "" with
    | s when not (String.IsNullOrWhiteSpace s) -> publish s
    // Non-interactive (CI): no token means skip the Marketplace publish rather
    // than block on a prompt. The GitHub release is still the deliverable.
    | _ when Option.isSome (Environment.environVarOrNone "CI") ->
        Trace.traceImportant "No vsce-token set; skipping VS Code Marketplace publish."
    | _ -> publish (UserInput.getUserPassword "VSCE Token: ")

let ensureGitUser user email =
    match CommandHelper.runGitCommand "." "config user.name" with
    | true, [ username ], _ when username = user -> ()
    | _, _, _ ->
        CommandHelper.directRunGitCommandAndFail "." $"config user.name %s{user}"
        CommandHelper.directRunGitCommandAndFail "." $"config user.email %s{email}"

let releaseGithub () =
    // Manual/local release bumps CHANGELOG, commits, pushes, and tags. Tag CI
    // skips this (commit and tag exist) and only drafts the release at the tag.
    if not isTagRelease then
        let user =
            match Environment.environVarOrDefault "github-user" "" with
            | s when not (String.IsNullOrWhiteSpace s) -> s
            | _ -> UserInput.getUserInput "Username: "

        let email =
            match Environment.environVarOrDefault "user-email" "" with
            | s when not (String.IsNullOrWhiteSpace s) -> s
            | _ -> UserInput.getUserInput "Email: "

        let remote =
            CommandHelper.getGitResult "" "remote -v"
            |> Seq.filter (fun (s: string) -> s.EndsWith("(push)"))
            |> Seq.tryFind (fun (s: string) -> s.Contains(gitOwner + "/" + gitName))
            |> function
                | None -> gitHome + "/" + gitName
                | Some(s: string) -> s.Split().[0]

        Staging.stageAll ""
        ensureGitUser user email
        Commit.exec "." $"Bump version to %s{releaseVersion}"
        Branches.pushBranch "" remote "main"
        Branches.tag "" releaseTag
        Branches.pushTag "" remote releaseTag

    let files = !!("./temp" </> "*.vsix")

    let token =
        match githubToken with
        | Some s -> s
        | _ ->
            failwith
                "please set the github_token environment variable to a github personal access token with repo access."

    let cl =
        GitHub.createClientWithToken token
        |> GitHub.draftNewRelease
            gitOwner
            gitName
            releaseTag
            isPreRelease
            releaseNotes

    (cl, files)
    ||> Seq.fold (fun acc e -> acc |> GitHub.uploadFile e)
    |> GitHub.publishDraft //releaseDraft
    |> Async.RunSynchronously

let initTargets () =

    Target.create "Clean" (fun _ ->
        Shell.cleanDir "./temp"
        Shell.cleanDir "./release/bin"
        Shell.copyFiles "release" [ "README.md"; "LICENSE.md" ]
        Shell.copyFile "release/CHANGELOG.md" "CHANGELOG.md")

    Target.create "NpmInstall" <| fun _ -> run npmTool.Value "install" "."

    Target.create "CopyDocs" (fun _ ->
        Shell.copyFiles "release" [ "README.md"; "LICENSE.md" ]
        Shell.copyFile "release/CHANGELOG.md" "CHANGELOG.md")

    // Dev builds deploy both engines so cwtools.engine can switch without
    // a rebuild. Cross-platform packaging stays Rust-only.
    Target.create "BuildServer" <| fun _ ->
        buildAndDeployRustServer ()
        buildAndDeployFSharpServer true

    Target.create "BuildServerDebug" <| fun _ ->
        buildAndDeployRustServer ()
        buildAndDeployFSharpServer false

    // PublishServer: Rust-only. Cross-compiling for win/osx is a separate step.
    Target.create "PublishServer" <| fun _ -> buildAndDeployRustServer ()

    Target.create "BuildClient" (fun _ ->
        let npx =
            match ProcessUtils.tryFindFileOnPath "npx" with
            | Some p -> p
            | None -> failwith "didn't find npx"

        let runNpx label args =
            CreateProcess.fromRawCommand npx args
            |> Proc.run
            |> (fun r ->
                if r.ExitCode <> 0 then
                    failwithf "%s fail" label)

        runNpx "tsc" [ "tsc"; "-p"; "./tsconfig.extension.json" ]
        runNpx "esbuild" [ "tsx"; "build/esbuild.ts" ])

    Target.create "CopyHtml" (fun _ -> !!("client/webview/*.css") |> Shell.copyFiles "release/bin/client/webview")

    Target.create "CopyTestSamples" (fun _ ->
        Shell.copyDir "release/bin/client/test/sample" "client/test/sample" (fun _ -> true))


    Target.create "BuildPackage" (fun _ -> buildPackage "release")

    Target.create "SetVersion" (fun _ -> setVersion "release")

    Target.create "PublishToGallery" (fun _ -> publishToGallery "release")

    Target.create "ReleaseGitHub" (fun _ -> releaseGithub ())


    Target.description "Assemble the extension"
    Target.create "PrePackage" ignore

    Target.create "PrepareClient" ignore

    // --------------------------------------------------------------------------------------
    // Run generator by default. Invoke 'build <Target>' to override
    // --------------------------------------------------------------------------------------
    Target.description "Build the requirements to run the extension locally"
    Target.create "QuickBuild" ignore
    Target.description "Build the requirements to run the extension locally, in debug mode"
    Target.create "QuickBuildDebug" ignore
    Target.description "Package into the vsix, but don't publish it"
    Target.create "DryRelease" ignore
    Target.description "Package into the vsix, and publish it"
    Target.create "Release" ignore
    Target.description "Package + publish a vsix whose server binaries are already staged (multi-platform CI release)"
    Target.create "ReleasePrebuilt" ignore


let buildTargetTree () =
    let (==>!) x y = x ==> y |> ignore

    //Clean only if we care about final output, so clean if DryRelease or Release

    //BuildClient doesn't change, and needs
    //PrepareClient gets everything up to date for the clientside and needs
    //- NpmInstall if deps have changed?
    //- BuildClient
    //-CopyDocs, CopyHtml

    //BuildServer is non-self-contained, using remote cwtools folder
    //BuildServerLocal is non-self-contained, using local cwtools folder
    //PublishServer is self-contained, all platforms

    //PrePackage copies client/server bin to extension dir

    //Release needs PublishServer


    "Clean" ?=> "NpmInstall"
    ==> "BuildClient"
    ==> "CopyDocs"
    ==> "CopyHtml"
    ==> "CopyTestSamples"
    ==> "PrepareClient"
    ==> "PrePackage"
    ==>! "BuildPackage"

    "PublishServer" ?=> "PrePackage" |> ignore
    "BuildServer" ?=> "PrePackage" |> ignore
    "BuildServerDebug" ?=> "PrePackage" |> ignore

    // Shared packaging + publishing chain. From SetVersion on it just packages
    // and ships whatever binaries are staged under release/bin/server, so both
    // release paths funnel through here.
    "SetVersion"
    ==> "BuildPackage"
    ==> "ReleaseGitHub"
    ==> "PublishToGallery"
    |> ignore

    // Full single-runner release: clean, build the Rust server, then package.
    // Clean and PublishServer are soft-ordered (?=>) before packaging; only
    // Release hard-requires them, so ReleasePrebuilt skips both.
    "Clean" ?=> "PublishServer" |> ignore
    "PublishServer" ?=> "SetVersion" |> ignore
    "Clean" ==> "Release" |> ignore
    "PublishServer" ==> "Release" |> ignore
    "PublishToGallery" ==>! "Release"

    // Multi-platform release: CI builds the per-platform Rust binaries and stages
    // them under release/bin/server/cwtools-server/<platform>/ before this runs,
    // so it only packages and publishes. No Clean, no server build.
    "PublishToGallery" ==>! "ReleasePrebuilt"

    // Clean wipes release/bin, so it must stay a soft prerequisite of
    // BuildPackage. A hard edge would delete the binaries ReleasePrebuilt stages
    // before packaging. DryRelease still cleans via its own edge below.
    "Clean" ?=> "BuildPackage" |> ignore
    "Clean" ==> "DryRelease" |> ignore
    "BuildPackage" ==>! "DryRelease"

    "BuildServer" ==>! "QuickBuild"

    "PrePackage" ==>! "QuickBuild"

    "BuildServerDebug" ==>! "QuickBuildDebug"

    "PrePackage" ==>! "QuickBuildDebug"

[<EntryPoint>]
let main argv =
    // Microsoft.Build.Logging.StructuredLogger.Strings.Initialize()
    argv
    |> Array.toList
    |> Context.FakeExecutionContext.Create false "build.fsx"
    |> Context.RuntimeContext.Fake
    |> Context.setExecutionContext

    initTargets ()
    buildTargetTree ()

    Target.runOrDefaultWithArguments "QuickBuild"
    0
