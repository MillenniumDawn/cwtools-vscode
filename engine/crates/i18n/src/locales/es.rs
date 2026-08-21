//! Spanish. Sorted by key id.

pub(crate) static UI: &[(&str, &str)] = &[
    ("action.createLocKey", "Crear la clave de localización {}"),
    ("action.didYouMean", "¿Querías decir '{}'?"),
    ("action.fixAll", "Corregir todo ({} autocorregibles)"),
    ("action.ignoreCode", "Ignorar {} en esta área de trabajo"),
    ("action.removeEmptyIf", "Quitar el if vacío"),
    ("action.removeEmptyLimit", "Quitar el limit vacío"),
    ("action.removeRedundant", "Quitar el {} redundante"),
    (
        "action.removeRedundantDefault",
        "Quitar el valor predeterminado redundante",
    ),
    (
        "action.removeUnnecessaryQuotes",
        "Quitar las comillas innecesarias",
    ),
    ("action.renameToSetName", "Cambiar el nombre a set_name"),
    (
        "command.cachesCleared",
        "Cachés vaciadas ({} archivos); {}.",
    ),
    (
        "command.cachesClearedWithErrors",
        "Cachés vaciadas ({} archivos) con {} error(es); {}. Fallidos: {}",
    ),
    (
        "command.noRulesDirectory",
        "No hay ninguna carpeta de reglas configurada; nada que recargar.",
    ),
    (
        "command.noRulesLoaded",
        "No se cargó ninguna regla desde {}; {}.",
    ),
    ("command.reindexCancelled", "Reindexado cancelado."),
    (
        "command.reindexInProgress",
        "Ya hay un reindexado en curso.",
    ),
    (
        "command.rulesReloaded",
        "Configuración de reglas recargada; {}.",
    ),
    ("command.workspaceReindexed", "Área de trabajo reindexada."),
    ("hover.description", "Descripción"),
    ("hover.localisation", "Localización"),
    ("hover.requiredScopes", "Scopes requeridos"),
    ("hover.resolvesTo", "Se resuelve en"),
    ("hover.scope", "Scope"),
    ("progress.cancelled", "Cancelado."),
    ("progress.discover", "Explorando el área de trabajo…"),
    (
        "progress.localisation",
        "Construyendo el índice de localización…",
    ),
    ("progress.parse", "Indexando el área de trabajo…"),
    ("progress.publish", "Publicando diagnósticos…"),
    ("progress.validate", "Validando el área de trabajo…"),
    ("progress.vanilla", "Indexando el juego base…"),
    (
        "status.reindexCancelledRebuilding",
        "reindexado cancelado, reconstruyendo en segundo plano",
    ),
    (
        "status.reindexPending",
        "reindexado todavía pendiente (hay otro escaneo en curso)",
    ),
    ("status.reindexed", "área de trabajo reindexada"),
    ("status.revalidated", "área de trabajo revalidada"),
    ("status.revalidationCancelled", "revalidación cancelada"),
    (
        "status.revalidationPending",
        "revalidación todavía pendiente (hay un escaneo en curso)",
    ),
    (
        "status.revalidationQueued",
        "revalidación en cola tras el escaneo en curso",
    ),
];
