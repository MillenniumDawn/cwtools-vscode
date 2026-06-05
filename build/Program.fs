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
let gitOwner = "cwtools"
let gitHome = "https://github.com/" + gitOwner

// The name of the project on GitHub
let gitName = "cwtools-vscode"


// Read additional information from the release notes document
let releaseNotesData = File.ReadAllLines "CHANGELOG.md" |> ReleaseNotes.parseAll

let release = List.head releaseNotesData

let githubToken = Environment.environVarOrNone "GITHUB_TOKEN"
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
// extension launches over stdio.
let rustWorkspace = "../cwtools/cwtools-rs"
let rustServerBinName =
    if Environment.isWindows then "cwtools-server.exe" else "cwtools-server"
let rustServerBin = rustWorkspace </> "target/release" </> rustServerBinName
// Deploy to the path the client actually loads first; the old
// bin/server/<platform>/"CWTools Server" path is only a legacy fallback.
let serverOutDir = releaseDir </> "bin/server/cwtools-server"
let deployedServerName = rustServerBinName

let buildAndDeployRustServer () =
    run "cargo" "build --release -p cwtools_lsp" rustWorkspace
    // Clean the server dir so stale F# .NET files (hostfxr, *.dll,
    // *.deps.json) don't linger next to the standalone Rust binary.
    Shell.cleanDir serverOutDir
    let dest = serverOutDir </> deployedServerName
    System.IO.File.Copy(rustServerBin, dest, true)
    if Environment.isUnix then
        System.IO.File.SetUnixFileMode(
            dest,
            UnixFileMode.UserRead ||| UnixFileMode.UserWrite ||| UnixFileMode.UserExecute
            ||| UnixFileMode.GroupRead ||| UnixFileMode.GroupExecute
            ||| UnixFileMode.OtherRead ||| UnixFileMode.OtherExecute
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
    run npxTool.Value "--yes @vscode/vsce package" dir

    !! $"%s{dir}/*.vsix" |> Seq.iter (Shell.moveFile "./temp/")

let setPackageJsonField (name: string) (value: string) releaseDir =
    let fileName = $"./%s{releaseDir}/package.json"
    let content = File.readAsString fileName
    let jsonObj = JsonDocument.Parse content
    let node = System.Text.Json.Nodes.JsonObject.Create jsonObj.RootElement
    node[name] <- value
    let opts = JsonSerializerOptions(WriteIndented = true, AllowTrailingCommas = false)
    File.WriteAllText(fileName, node.ToJsonString(opts))

let setVersion (release: ReleaseNotes.ReleaseNotes) releaseDir =
    let versionString = $"%O{release.NugetVersion}"
    setPackageJsonField "version" versionString releaseDir

let publishToGallery releaseDir =
    let token =
        match Environment.environVarOrDefault "vsce-token" "" with
        | s when not (String.IsNullOrWhiteSpace s) -> s
        | _ -> UserInput.getUserPassword "VSCE Token: "

    Process.killAllByName "npx"
    run npxTool.Value $"@vscode/vsce publish --pat %s{token}" releaseDir

let ensureGitUser user email =
    match CommandHelper.runGitCommand "." "config user.name" with
    | true, [ username ], _ when username = user -> ()
    | _, _, _ ->
        CommandHelper.directRunGitCommandAndFail "." $"config user.name %s{user}"
        CommandHelper.directRunGitCommandAndFail "." $"config user.email %s{email}"

let releaseGithub (release: ReleaseNotes.ReleaseNotes) =
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
    Commit.exec "." $"Bump version to %s{release.NugetVersion}"
    Branches.pushBranch "" remote "main"
    Branches.tag "" release.NugetVersion
    Branches.pushTag "" remote release.NugetVersion

    let files = !!("./temp" </> "*.vsix")

    let token =
        match githubToken with
        | Some s -> s
        | _ ->
            failwith
                "please set the github_token environment variable to a github personal access token with repo access."

    // release on github
    let cl =
        GitHub.createClientWithToken token
        |> GitHub.draftNewRelease
            gitOwner
            gitName
            release.NugetVersion
            (release.SemVer.PreRelease <> None)
            release.Notes

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

    Target.create "PackageNpmInstall"
    <| fun _ -> run npmTool.Value "install" "release"

    Target.create "CopyDocs" (fun _ ->
        Shell.copyFiles "release" [ "README.md"; "LICENSE.md" ]
        Shell.copyFile "release/CHANGELOG.md" "CHANGELOG.md")

    let publishParams (framework: string) =
        fun (p: DotNet.PublishOptions) ->
            { p with
                Common =
                    { p.Common with
                        CustomParams = Some "--self-contained true /p:PublishReadyToRun=true /p:UseLocalCwtools=False" }
                OutputPath = Some(releaseDir </> "bin/server" </> framework)
                Runtime = Some framework
                Configuration = DotNet.BuildConfiguration.Release
                MSBuildParams =
                    { MSBuild.CliArguments.Create() with
                        DisableInternalBinLog = true } }

    let buildParams (release: bool) =
        fun (b: DotNet.BuildOptions) ->
            { b with
                OutputPath = Some(releaseDir </> "bin/server" </> platformShortCode)
                Configuration =
                    if release then
                        DotNet.BuildConfiguration.Release
                    else
                        DotNet.BuildConfiguration.Debug
                MSBuildParams =
                    { MSBuild.CliArguments.Create() with
                        DisableInternalBinLog = true } }

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
        match ProcessUtils.tryFindFileOnPath "npx" with
        | Some tsc ->
            CreateProcess.fromRawCommand tsc [ "tsc"; "-p"; "./tsconfig.extension.json" ]
            |> Proc.run
            |> (fun r ->
                if r.ExitCode <> 0 then
                    failwith "tsc fail")
        | _ -> failwith "didn't find tsc"

        match ProcessUtils.tryFindFileOnPath "npx" with
        | Some tsc ->
            CreateProcess.fromRawCommand tsc [ "rollup"; "-c"; "-o"; "./release/bin/client/webview/graph.js" ]
            |> Proc.run
            |> (fun r ->
                if r.ExitCode <> 0 then
                    failwith "rollup fail")
        | _ -> failwith "didn't find rollup")

    Target.create "CopyHtml" (fun _ -> !!("client/webview/*.css") |> Shell.copyFiles "release/bin/client/webview")

    Target.create "CopyTestSamples" (fun _ ->
        Shell.copyDir "release/bin/client/test/sample" "client/test/sample" (fun _ -> true))


    Target.create "BuildPackage" (fun _ -> buildPackage "release")

    Target.create "SetVersion" (fun _ -> setVersion release "release")

    Target.create "PublishToGallery" (fun _ -> publishToGallery "release")

    Target.create "ReleaseGitHub" (fun _ -> releaseGithub release)


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

    // Shared packaging + publishing steps. From SetVersion on, the chain is
    // engine-agnostic: it just packages and ships whatever server binaries are
    // already staged under release/bin/server, so both release paths funnel
    // through here.
    "SetVersion"
    ==> "PackageNpmInstall"
    ==> "BuildPackage"
    ==> "ReleaseGitHub"
    ==> "PublishToGallery"
    |> ignore

    // Full single-runner release: clean, build the Rust server here, then
    // package. Clean and PublishServer are only ordered before the packaging
    // chain (soft ?=>); only Release hard-requires them, so ReleasePrebuilt can
    // skip both.
    "Clean" ?=> "PublishServer" |> ignore
    "PublishServer" ?=> "SetVersion" |> ignore
    "Clean" ==> "Release" |> ignore
    "PublishServer" ==> "Release" |> ignore
    "PublishToGallery" ==>! "Release"

    // Multi-platform release: the win/osx/linux Rust binaries are built per
    // platform in CI and downloaded into
    // release/bin/server/cwtools-server/<platform>/ before this runs, so it only
    // packages + publishes — no Clean, no server build.
    "PublishToGallery" ==>! "ReleasePrebuilt"

    "Clean" ==> "BuildPackage" ==>! "DryRelease"

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
