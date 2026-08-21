//! French. Sorted by key id.

pub(crate) static UI: &[(&str, &str)] = &[
    ("action.createLocKey", "Créer la clé de localisation {}"),
    ("action.didYouMean", "Vouliez-vous dire '{}' ?"),
    ("action.fixAll", "Tout corriger ({} auto-corrigeables)"),
    ("action.ignoreCode", "Ignorer {} dans cet espace de travail"),
    ("action.removeEmptyIf", "Supprimer le if vide"),
    ("action.removeEmptyLimit", "Supprimer le limit vide"),
    ("action.removeRedundant", "Supprimer le {} redondant"),
    (
        "action.removeRedundantDefault",
        "Supprimer la valeur par défaut redondante",
    ),
    (
        "action.removeUnnecessaryQuotes",
        "Supprimer les guillemets inutiles",
    ),
    ("action.renameToSetName", "Renommer en set_name"),
    ("command.cachesCleared", "Caches vidés ({} fichiers) ; {}."),
    (
        "command.cachesClearedWithErrors",
        "Caches vidés ({} fichiers) avec {} erreur(s) ; {}. Échecs : {}",
    ),
    (
        "command.noRulesDirectory",
        "Aucun dossier de règles configuré ; rien à recharger.",
    ),
    (
        "command.noRulesLoaded",
        "Aucune règle chargée depuis {} ; {}.",
    ),
    ("command.reindexCancelled", "Réindexation annulée."),
    (
        "command.reindexInProgress",
        "Une réindexation est déjà en cours.",
    ),
    (
        "command.rulesReloaded",
        "Configuration des règles rechargée ; {}.",
    ),
    ("command.workspaceReindexed", "Espace de travail réindexé."),
    ("hover.description", "Description"),
    ("hover.localisation", "Localisation"),
    ("hover.requiredScopes", "Scopes requis"),
    ("hover.resolvesTo", "Se résout en"),
    ("hover.scope", "Scope"),
    ("progress.cancelled", "Annulé."),
    ("progress.discover", "Analyse de l'espace de travail…"),
    (
        "progress.localisation",
        "Construction de l'index de localisation…",
    ),
    ("progress.parse", "Indexation de l'espace de travail…"),
    ("progress.publish", "Publication des diagnostics…"),
    ("progress.validate", "Validation de l'espace de travail…"),
    ("progress.vanilla", "Indexation du jeu de base…"),
    (
        "status.reindexCancelledRebuilding",
        "réindexation annulée, reconstruction en arrière-plan",
    ),
    (
        "status.reindexPending",
        "réindexation encore en attente (une autre analyse est en cours)",
    ),
    ("status.reindexed", "espace de travail réindexé"),
    ("status.revalidated", "espace de travail revalidé"),
    ("status.revalidationCancelled", "revalidation annulée"),
    (
        "status.revalidationPending",
        "revalidation encore en attente (une analyse est en cours)",
    ),
    (
        "status.revalidationQueued",
        "revalidation mise en file derrière l'analyse en cours",
    ),
];
