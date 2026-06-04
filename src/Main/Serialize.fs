module Main.Serialize

open System.IO
open CWTools.Games.Files
open CWTools.Serializer

let private serializeToCache serializer outName folder cacheDirectory =
    serializer
        { WorkspaceDirectory.name = "vanilla"
          path = folder }
        (Some(Path.Combine(cacheDirectory, outName)))
        CWTools.CompressionOptions.NoCompression
    |> ignore

let serializeSTL = serializeToCache CWTools.Serializer.serializeSTL "stl.cwb"
let serializeEU4 = serializeToCache CWTools.Serializer.serializeEU4 "eu4.cwb"
let serializeHOI4 = serializeToCache CWTools.Serializer.serializeHOI4 "hoi4.cwb"
let serializeCK2 = serializeToCache CWTools.Serializer.serializeCK2 "ck2.cwb"
let serializeIR = serializeToCache CWTools.Serializer.serializeIR "ir.cwb"
let serializeVIC2 = serializeToCache CWTools.Serializer.serializeVIC2 "vic2.cwb"
let serializeCK3 = serializeToCache CWTools.Serializer.serializeCK3 "ck3.cwb"
let serializeVIC3 = serializeToCache CWTools.Serializer.serializeVIC3 "vic3.cwb"
let serializeEU5 = serializeToCache CWTools.Serializer.serializeEU5 "eu5.cwb"

let deserialize path =
    try
        let result = deserialize path
        let entities = fst result
        let files = snd result
        CWTools.Utilities.Utils.logInfo (sprintf "Loaded cache from %s (%d entities, %d files)" path entities.Length files.Length)
        result
    with ex ->
        CWTools.Utilities.Utils.logWarning (sprintf "Failed to load cache from %s: %s (vanilla definitions will be missing)" path ex.Message)
        [], []
