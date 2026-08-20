//! French diagnostic templates. Sorted by code id.

pub(crate) static TEMPLATES: &[(&str, &str)] = &[
    ("CW001", "Erreur d'analyse du fichier de localisation : {}"),
    (
        "CW100",
        "La clé de localisation {} n'est pas définie pour {}",
    ),
    (
        "CW104",
        "Trigger {} utilisé dans le mauvais scope. Dans {} alors que {} était attendu",
    ),
    (
        "CW105",
        "Effet {} utilisé dans le mauvais scope. Dans {} alors que {} était attendu",
    ),
    (
        "CW106",
        "Commande de scope {} utilisée dans le mauvais scope. Dans {} alors que {} était attendu",
    ),
    (
        "CW107",
        "Il manque à cet événement mean_time_to_happen, is_triggered_only, fire_only_once ou trigger={always=no}. Performance : l'événement peut se déclencher à chaque tick.",
    ),
    (
        "CW108",
        "Il manque le \"area\" obligatoire à ce research_leader",
    ),
    (
        "CW109",
        "Ce research_leader utilise l'area {} alors que la technologie utilise l'area {}",
    ),
    ("CW110", "Aucune catégorie trouvée pour cette technologie"),
    (
        "CW113",
        "Fichier {} introuvable ; la casse est prise en compte",
    ),
    (
        "CW120",
        "Le trigger {} peut devenir un pretrigger (voir l'action de code pour le corriger)",
    ),
    ("CW121", "Ce 'if' ne contient aucun effet"),
    (
        "CW122",
        "La clé de localisation {} ne devrait pas être entre guillemets en usage inline, cela peut provoquer un comportement inattendu",
    ),
    (
        "CW220",
        "{} ou un événement qu'il appelle a besoin des cibles d'événement {}, mais elles ne sont définies ni ici ni par tous les chemins menant ici",
    ),
    (
        "CW221",
        "{} ou un événement qu'il appelle a besoin des cibles d'événement {}, mais elles ne sont peut-être pas toujours définies ici ou par tous les chemins menant ici",
    ),
    ("CW222", "L'identifiant d'événement {} n'est pas défini"),
    (
        "CW223",
        "N'utilisez pas NOT avec plusieurs enfants ; remplacez-le par NOR ou NAND pour lever l'ambiguïté",
    ),
    (
        "CW223.hoi4",
        "NOT avec plusieurs enfants se comporte comme NOR (vrai seulement si chaque enfant est faux). Rendez l'intention explicite : NOT = { OR = { ... } } pour NOR, ou NOT = { AND = { ... } } pour NAND.",
    ),
    (
        "CW225",
        "La clé de localisation \"{}\" référence \"{}\", qui n'existe pas dans {}",
    ),
    (
        "CW226",
        "La clé de localisation \"{}\" utilise la commande \"{}\", qui n'existe pas",
    ),
    ("CW227", "Le modèle de section {} est introuvable"),
    ("CW228", "Le modèle de section {} n'a pas d'emplacement {}"),
    ("CW229", "Le modèle de composant {} est introuvable"),
    (
        "CW230",
        "Le composant et l'emplacement ne correspondent pas : l'emplacement {} a la taille {} et le composant {} la taille {}",
    ),
    ("CW231", "La technologie {} n'est pas utilisée"),
    ("CW233", "L'entité {} n'est pas définie"),
    (
        "CW234",
        "La clé de localisation {} n'est qu'un texte provisoire pour {}",
    ),
    (
        "CW235",
        "Le modificateur {} vaut 0. Les modificateurs sont additifs, cela ne fait donc probablement rien",
    ),
    (
        "CW236",
        "Le if/else imbriqué dans les effets est déprécié depuis la 2.1 et sera retiré dans une version future",
    ),
    (
        "CW237",
        "La 2.1 a changé le comportement du if = { if else } imbriqué dans les effets. Vérifiez que cela fonctionne toujours comme prévu",
    ),
    ("CW238", "Ce else/else_if n'a pas de if avant lui"),
    (
        "CW239",
        "{} de type {} n'est utilisé nulle part alors qu'il devrait l'être",
    ),
    (
        "CW243",
        "La cible \"{}\" a le mauvais scope. Elle est {} alors que {} est attendu",
    ),
    (
        "CW244",
        "{} n'est pas une cible. Une cible dans le ou les scopes {} est attendue",
    ),
    (
        "CW245",
        "Erreur dans la cible. Le lien {} a été utilisé dans le scope {} alors que {} était attendu",
    ),
    ("CW246", "La variable {} n'a jamais été définie"),
    (
        "CW247",
        "Trigger/effet/modificateur {} utilisé dans le mauvais scope. Dans {} alors que {} est attendu",
    ),
    ("CW248", "Commande de scope {} invalide"),
    ("CW251", "Ce {} est inutile"),
    ("CW253", "Préférez \"set_name\" par souci de cohérence"),
    (
        "CW254",
        "Les fichiers de localisation doivent être en UTF-8 avec BOM ; celui-ci ne l'est pas",
    ),
    (
        "CW255",
        "Le nom du fichier de localisation devrait contenir \"l_language.yml\", idéalement à la fin",
    ),
    (
        "CW256",
        "Un fichier de localisation devrait commencer par \"l_language:\" sur la première ligne (ou par un commentaire)",
    ),
    (
        "CW257",
        "Le nom du fichier de localisation indique la langue {}, différente de la langue {} de l'en-tête",
    ),
    (
        "CW259",
        "Cette chaîne de localisation se référence elle-même",
    ),
    (
        "CW260",
        "Commande loc {} utilisée dans le mauvais scope. Dans {} alors que {} était attendu",
    ),
    ("CW261", "La clé {} de type {} est définie plusieurs fois"),
    (
        "CW266",
        "La clé de localisation {} utilise la commande {}, qui n'existe pas dans le type de données {}.",
    ),
    ("CW267", "Une valeur de type {} était attendue, reçu {}"),
    (
        "CW268",
        "La clé de localisation {} ne commence ni ne finit par des guillemets doubles",
    ),
    (
        "CW269",
        "Optimisez en fusionnant ceci avec {} à l'aide de {}",
    ),
    (
        "CW270",
        "Valeur trop petite : seules 3 décimales sont prises en charge ici",
    ),
    ("CW271", "Un entier était attendu"),
    (
        "CW273",
        "Le type de modificateur {} est utilisé mais n'est pas défini",
    ),
    (
        "CW275",
        "La valeur de localisation de {} contient des caractères inattendus et peut mal s'afficher",
    ),
    (
        "CW276",
        "La clé de localisation {} contient des caractères invalides (les espaces et les caractères spéciaux sont interdits)",
    ),
    (
        "CW277",
        "La validation s'est arrêtée à la limite de branches d'alias",
    ),
    (
        "CW280",
        "{} = { always = ... } correspond à la valeur par défaut et peut être supprimé",
    ),
    ("CW281", "Ce 'limit' ne contient aucun trigger"),
    (
        "CW282",
        "C'est la valeur par défaut ({}) et elle peut être omise",
    ),
    ("CW500", "Type '{}' introuvable"),
    ("CW600", "Le fichier de règles n'a pas pu être lu : {}"),
    ("CW601", "La règle référence un {} `{}` non défini"),
];
