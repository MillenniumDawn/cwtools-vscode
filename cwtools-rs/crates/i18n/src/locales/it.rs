//! Italian. Sorted by key id.

pub(crate) static UI: &[(&str, &str)] = &[
    ("action.createLocKey", "Crea la chiave di localizzazione {}"),
    ("action.didYouMean", "Intendevi '{}'?"),
    (
        "action.fixAll",
        "Correggi tutto ({} correggibili in automatico)",
    ),
    ("action.ignoreCode", "Ignora {} in quest'area di lavoro"),
    ("action.removeEmptyIf", "Rimuovi l'if vuoto"),
    ("action.removeEmptyLimit", "Rimuovi il limit vuoto"),
    ("action.removeRedundant", "Rimuovi il {} ridondante"),
    (
        "action.removeRedundantDefault",
        "Rimuovi il valore predefinito ridondante",
    ),
    (
        "action.removeUnnecessaryQuotes",
        "Rimuovi le virgolette superflue",
    ),
    ("action.renameToSetName", "Rinomina in set_name"),
    ("command.cachesCleared", "Cache svuotate ({} file); {}."),
    (
        "command.cachesClearedWithErrors",
        "Cache svuotate ({} file) con {} errore/i; {}. Non riusciti: {}",
    ),
    (
        "command.noRulesDirectory",
        "Nessuna cartella di regole configurata; niente da ricaricare.",
    ),
    (
        "command.noRulesLoaded",
        "Nessuna regola caricata da {}; {}.",
    ),
    ("command.reindexCancelled", "Reindicizzazione annullata."),
    (
        "command.reindexInProgress",
        "C'è già una reindicizzazione in corso.",
    ),
    (
        "command.rulesReloaded",
        "Configurazione delle regole ricaricata; {}.",
    ),
    (
        "command.workspaceReindexed",
        "Area di lavoro reindicizzata.",
    ),
    ("hover.description", "Descrizione"),
    ("hover.localisation", "Localizzazione"),
    ("hover.requiredScopes", "Scope richiesti"),
    ("hover.resolvesTo", "Si risolve in"),
    ("hover.scope", "Scope"),
    ("progress.cancelled", "Annullato."),
    ("progress.discover", "Scansione dell'area di lavoro…"),
    (
        "progress.localisation",
        "Costruzione dell'indice di localizzazione…",
    ),
    ("progress.parse", "Indicizzazione dell'area di lavoro…"),
    ("progress.publish", "Pubblicazione delle diagnostiche…"),
    ("progress.validate", "Validazione dell'area di lavoro…"),
    ("progress.vanilla", "Indicizzazione del gioco base…"),
    (
        "status.reindexCancelledRebuilding",
        "reindicizzazione annullata, ricostruzione in background",
    ),
    (
        "status.reindexPending",
        "reindicizzazione ancora in attesa (è in corso un'altra scansione)",
    ),
    ("status.reindexed", "area di lavoro reindicizzata"),
    ("status.revalidated", "area di lavoro rivalidata"),
    ("status.revalidationCancelled", "rivalidazione annullata"),
    (
        "status.revalidationPending",
        "rivalidazione ancora in attesa (è in corso una scansione)",
    ),
    (
        "status.revalidationQueued",
        "rivalidazione in coda dopo la scansione in corso",
    ),
];
