//! German. Sorted by key id.

pub(crate) static UI: &[(&str, &str)] = &[
    ("action.createLocKey", "Lokalisierungsschlüssel {} anlegen"),
    ("action.didYouMean", "Meintest du '{}'?"),
    ("action.fixAll", "Alle beheben ({} automatisch behebbar)"),
    (
        "action.ignoreCode",
        "{} in diesem Arbeitsbereich ignorieren",
    ),
    ("action.removeEmptyIf", "Leeres if entfernen"),
    ("action.removeEmptyLimit", "Leeres limit entfernen"),
    ("action.removeRedundant", "Überflüssiges {} entfernen"),
    (
        "action.removeRedundantDefault",
        "Überflüssigen Standardwert entfernen",
    ),
    (
        "action.removeUnnecessaryQuotes",
        "Unnötige Anführungszeichen entfernen",
    ),
    ("action.renameToSetName", "In set_name umbenennen"),
    ("command.cachesCleared", "Caches geleert ({} Dateien); {}."),
    (
        "command.cachesClearedWithErrors",
        "Caches geleert ({} Dateien) mit {} Fehler(n); {}. Fehlgeschlagen: {}",
    ),
    (
        "command.noRulesDirectory",
        "Kein Regelverzeichnis konfiguriert; nichts zum Neuladen.",
    ),
    ("command.noRulesLoaded", "Keine Regeln aus {} geladen; {}."),
    ("command.reindexCancelled", "Neuindizierung abgebrochen."),
    (
        "command.reindexInProgress",
        "Es läuft bereits eine Neuindizierung.",
    ),
    (
        "command.rulesReloaded",
        "Regelkonfiguration neu geladen; {}.",
    ),
    (
        "command.workspaceReindexed",
        "Arbeitsbereich neu indiziert.",
    ),
    ("hover.description", "Beschreibung"),
    ("hover.localisation", "Lokalisierung"),
    ("hover.requiredScopes", "Erforderliche Scopes"),
    ("hover.resolvesTo", "Löst auf zu"),
    ("hover.scope", "Scope"),
    ("progress.cancelled", "Abgebrochen."),
    ("progress.discover", "Arbeitsbereich wird durchsucht…"),
    (
        "progress.localisation",
        "Lokalisierungsindex wird aufgebaut…",
    ),
    ("progress.parse", "Arbeitsbereich wird indiziert…"),
    ("progress.publish", "Diagnosen werden veröffentlicht…"),
    ("progress.validate", "Arbeitsbereich wird geprüft…"),
    ("progress.vanilla", "Grundspiel wird indiziert…"),
    (
        "status.reindexCancelledRebuilding",
        "Neuindizierung abgebrochen, Wiederaufbau läuft im Hintergrund",
    ),
    (
        "status.reindexPending",
        "Neuindizierung steht noch aus (ein anderer Scan läuft)",
    ),
    ("status.reindexed", "Arbeitsbereich neu indiziert"),
    ("status.revalidated", "Arbeitsbereich neu geprüft"),
    ("status.revalidationCancelled", "Neuprüfung abgebrochen"),
    (
        "status.revalidationPending",
        "Neuprüfung steht noch aus (ein Scan läuft)",
    ),
    (
        "status.revalidationQueued",
        "Neuprüfung ist hinter dem laufenden Scan eingereiht",
    ),
];
