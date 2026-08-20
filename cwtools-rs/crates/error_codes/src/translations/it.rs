//! Italian diagnostic templates. Sorted by code id.

pub(crate) static TEMPLATES: &[(&str, &str)] = &[
    ("CW001", "Errore di analisi del file di localizzazione: {}"),
    (
        "CW100",
        "La chiave di localizzazione {} non è definita per {}",
    ),
    (
        "CW104",
        "Il trigger {} è usato in uno scope sbagliato. È in {} ma era atteso {}",
    ),
    (
        "CW105",
        "L'effetto {} è usato in uno scope sbagliato. È in {} ma era atteso {}",
    ),
    (
        "CW106",
        "Il comando di scope {} è usato in uno scope sbagliato. È in {} ma era atteso {}",
    ),
    (
        "CW107",
        "A questo evento manca mean_time_to_happen, is_triggered_only, fire_only_once oppure trigger={always=no}. Prestazioni: l'evento può scattare a ogni tick.",
    ),
    (
        "CW108",
        "A questo research_leader manca l'\"area\" obbligatoria",
    ),
    (
        "CW109",
        "Questo research_leader usa l'area {} mentre la tecnologia usa l'area {}",
    ),
    ("CW110", "Nessuna categoria trovata per questa tecnologia"),
    (
        "CW113",
        "File {} non trovato; maiuscole e minuscole contano",
    ),
    (
        "CW120",
        "Il trigger {} può diventare un pretrigger (usa l'azione di codice per correggerlo)",
    ),
    ("CW121", "Questo 'if' non contiene effetti"),
    (
        "CW122",
        "La chiave di localizzazione {} non dovrebbe stare fra virgolette in uso inline: può causare comportamenti inattesi",
    ),
    (
        "CW220",
        "{} o un evento che richiama ha bisogno degli event target {}, ma non vengono impostati qui né da tutti i percorsi che portano qui",
    ),
    (
        "CW221",
        "{} o un evento che richiama ha bisogno degli event target {}, ma potrebbero non essere sempre impostati qui né da tutti i percorsi che portano qui",
    ),
    ("CW222", "L'id evento {} non è definito"),
    (
        "CW223",
        "Non usare NOT con più figli: sostituiscilo con NOR o NAND per togliere l'ambiguità",
    ),
    (
        "CW223.hoi4",
        "NOT con più figli si comporta come NOR (vero solo se ogni figlio è falso). Rendi esplicita l'intenzione: NOT = { OR = { ... } } per NOR, oppure NOT = { AND = { ... } } per NAND.",
    ),
    (
        "CW225",
        "La chiave di localizzazione \"{}\" fa riferimento a \"{}\", che non esiste in {}",
    ),
    (
        "CW226",
        "La chiave di localizzazione \"{}\" usa il comando \"{}\", che non esiste",
    ),
    ("CW227", "Il template di sezione {} non si trova"),
    ("CW228", "Il template di sezione {} non ha lo slot {}"),
    ("CW229", "Il template di componente {} non si trova"),
    (
        "CW230",
        "Componente e slot non combaciano: lo slot {} ha dimensione {} e il componente {} ha dimensione {}",
    ),
    ("CW231", "La tecnologia {} non è usata"),
    ("CW233", "L'entità {} non è definita"),
    (
        "CW234",
        "La chiave di localizzazione {} è solo un segnaposto per {}",
    ),
    (
        "CW235",
        "Il modificatore {} vale 0. I modificatori sono additivi, quindi con ogni probabilità non fa nulla",
    ),
    (
        "CW236",
        "L'if/else annidato negli effetti è deprecato dalla 2.1 e sarà rimosso in una versione futura",
    ),
    (
        "CW237",
        "La 2.1 ha cambiato il comportamento dell'if = { if else } annidato negli effetti. Verifica che funzioni ancora come previsto",
    ),
    ("CW238", "A questo else/else_if manca un if che lo preceda"),
    (
        "CW239",
        "{} di tipo {} non è usato da nessuna parte, benché ci si aspetti che lo sia",
    ),
    (
        "CW243",
        "Il target \"{}\" ha lo scope sbagliato. È {} ma ci si aspetta {}",
    ),
    (
        "CW244",
        "{} non è un target. Era atteso un target nello scope o negli scope {}",
    ),
    (
        "CW245",
        "Errore nel target. Il link {} è stato usato nello scope {} ma era atteso {}",
    ),
    ("CW246", "La variabile {} non è mai stata impostata"),
    (
        "CW247",
        "Trigger/effetto/modificatore {} usato nello scope sbagliato. È in {} ma ci si aspetta {}",
    ),
    ("CW248", "Comando di scope {} non valido"),
    ("CW251", "Questo {} è superfluo"),
    ("CW253", "Per coerenza è meglio usare \"set_name\""),
    (
        "CW254",
        "I file di localizzazione devono essere UTF-8 con BOM; questo non lo è",
    ),
    (
        "CW255",
        "Il nome del file di localizzazione dovrebbe contenere \"l_language.yml\", idealmente in fondo",
    ),
    (
        "CW256",
        "Un file di localizzazione dovrebbe iniziare con \"l_language:\" sulla prima riga (o con un commento)",
    ),
    (
        "CW257",
        "Il nome del file di localizzazione indica la lingua {}, diversa dalla lingua {} dell'intestazione",
    ),
    (
        "CW259",
        "Questa stringa di localizzazione fa riferimento a sé stessa",
    ),
    (
        "CW260",
        "Comando loc {} usato nello scope sbagliato. È in {} ma era atteso {}",
    ),
    ("CW261", "La chiave {} di tipo {} è definita più volte"),
    (
        "CW266",
        "La chiave di localizzazione {} usa il comando {}, che non esiste nel tipo di dati {}.",
    ),
    ("CW267", "Era atteso un valore di tipo {}, trovato {}"),
    (
        "CW268",
        "La chiave di localizzazione {} non inizia e non finisce con virgolette doppie",
    ),
    ("CW269", "Ottimizza unendo questo a {} usando {}"),
    (
        "CW270",
        "Valore troppo piccolo: qui sono supportati solo 3 decimali",
    ),
    ("CW271", "Era atteso un numero intero"),
    (
        "CW273",
        "Il tipo di modificatore {} è usato ma non è definito",
    ),
    (
        "CW275",
        "Il valore di localizzazione di {} contiene caratteri inattesi e potrebbe non essere reso correttamente",
    ),
    (
        "CW276",
        "La chiave di localizzazione {} contiene caratteri non validi (spazi e caratteri speciali non sono ammessi)",
    ),
    (
        "CW277",
        "La validazione si è fermata al limite di rami degli alias",
    ),
    (
        "CW280",
        "{} = { always = ... } coincide con il valore predefinito e si può togliere",
    ),
    ("CW281", "Questo 'limit' non contiene trigger"),
    (
        "CW282",
        "Questo è il valore predefinito ({}) e si può omettere",
    ),
    ("CW500", "Tipo '{}' non trovato"),
    ("CW600", "Impossibile leggere il file di regole: {}"),
    (
        "CW601",
        "La regola fa riferimento a un {} `{}` non definito",
    ),
];
