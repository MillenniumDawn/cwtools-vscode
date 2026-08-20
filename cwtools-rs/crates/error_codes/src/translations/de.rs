//! German diagnostic templates. Sorted by code id.

pub(crate) static TEMPLATES: &[(&str, &str)] = &[
    ("CW001", "Fehler beim Parsen der Lokalisierungsdatei: {}"),
    (
        "CW100",
        "Lokalisierungsschlüssel {} ist für {} nicht definiert",
    ),
    (
        "CW104",
        "Trigger {} im falschen Scope verwendet. In {}, erwartet wurde {}",
    ),
    (
        "CW105",
        "Effekt {} im falschen Scope verwendet. In {}, erwartet wurde {}",
    ),
    (
        "CW106",
        "Scope-Befehl {} im falschen Scope verwendet. In {}, erwartet wurde {}",
    ),
    (
        "CW107",
        "Dem Event fehlt mean_time_to_happen, is_triggered_only, fire_only_once oder trigger={always=no}. Performance: das Event kann in jedem Tick auslösen.",
    ),
    ("CW108", "Diesem research_leader fehlt das nötige \"area\""),
    (
        "CW109",
        "Dieser research_leader nutzt area {}, die Technologie nutzt aber area {}",
    ),
    ("CW110", "Zu dieser Technologie gibt es keine Kategorie"),
    (
        "CW113",
        "Datei {} nicht gefunden; die Schreibweise zählt (Groß-/Kleinschreibung)",
    ),
    (
        "CW120",
        "Trigger {} kann ein Pretrigger werden (die Code-Aktion erledigt das)",
    ),
    ("CW121", "Dieses 'if' enthält keine Effekte"),
    (
        "CW122",
        "Lokalisierungsschlüssel {} sollte inline nicht in Anführungszeichen stehen, das kann zu unerwartetem Verhalten führen",
    ),
    (
        "CW220",
        "{} oder ein aufgerufenes Event braucht die Event-Ziele {}, sie werden aber weder hier noch auf allen Wegen hierher gesetzt",
    ),
    (
        "CW221",
        "{} oder ein aufgerufenes Event braucht die Event-Ziele {}, sie werden aber möglicherweise nicht immer hier oder auf allen Wegen hierher gesetzt",
    ),
    ("CW222", "Die Event-ID {} ist nicht definiert"),
    (
        "CW223",
        "NOT nicht mit mehreren Kindern verwenden; nutze stattdessen NOR oder NAND, damit es eindeutig ist",
    ),
    (
        "CW223.hoi4",
        "NOT mit mehreren Kindern wirkt wie NOR (wahr nur, wenn jedes Kind falsch ist). Mach die Absicht sichtbar: NOT = { OR = { ... } } für NOR, oder NOT = { AND = { ... } } für NAND.",
    ),
    (
        "CW225",
        "Lokalisierungsschlüssel \"{}\" verweist auf \"{}\", das es in {} nicht gibt",
    ),
    (
        "CW226",
        "Lokalisierungsschlüssel \"{}\" nutzt den Befehl \"{}\", den es nicht gibt",
    ),
    ("CW227", "Section-Template {} ist nicht auffindbar"),
    ("CW228", "Section-Template {} hat keinen Slot {}"),
    ("CW229", "Component-Template {} ist nicht auffindbar"),
    (
        "CW230",
        "Component und Slot passen nicht zusammen: Slot {} hat Größe {}, Component {} hat Größe {}",
    ),
    ("CW231", "Technologie {} wird nicht verwendet"),
    ("CW233", "Entität {} ist nicht definiert"),
    (
        "CW234",
        "Lokalisierungsschlüssel {} ist nur ein Platzhalter für {}",
    ),
    (
        "CW235",
        "Modifier {} hat den Wert 0. Modifier sind additiv, das tut also vermutlich nichts",
    ),
    (
        "CW236",
        "Verschachteltes if/else in Effekten ist seit 2.1 veraltet und wird in einer späteren Version entfernt",
    ),
    (
        "CW237",
        "2.1 hat das Verhalten von verschachteltem if = { if else } in Effekten geändert. Prüfe, ob das noch wie gedacht funktioniert",
    ),
    ("CW238", "Zu diesem else/else_if fehlt ein vorangehendes if"),
    (
        "CW239",
        "{} vom Typ {} wird nirgends verwendet, obwohl das erwartet wird",
    ),
    (
        "CW243",
        "Ziel \"{}\" hat den falschen Scope. Ist {}, erwartet wird {}",
    ),
    (
        "CW244",
        "{} ist kein Ziel. Erwartet wird ein Ziel im Scope {}",
    ),
    (
        "CW245",
        "Fehler im Ziel. Link {} wurde im Scope {} verwendet, erwartet wurde {}",
    ),
    ("CW246", "Die Variable {} wurde nie gesetzt"),
    (
        "CW247",
        "Trigger/Effekt/Modifier {} im falschen Scope verwendet. In {}, erwartet wird {}",
    ),
    ("CW248", "Ungültiger Scope-Befehl {}"),
    ("CW251", "Dieses {} ist überflüssig"),
    (
        "CW253",
        "Nutze der Einheitlichkeit halber besser \"set_name\"",
    ),
    (
        "CW254",
        "Lokalisierungsdateien müssen UTF-8 mit BOM sein; diese ist es nicht",
    ),
    (
        "CW255",
        "Der Name der Lokalisierungsdatei sollte \"l_language.yml\" enthalten und idealerweise damit enden",
    ),
    (
        "CW256",
        "Eine Lokalisierungsdatei sollte in der ersten Zeile mit \"l_language:\" beginnen (oder mit einem Kommentar)",
    ),
    (
        "CW257",
        "Der Dateiname der Lokalisierungsdatei nennt die Sprache {}, der Header aber {}",
    ),
    (
        "CW259",
        "Dieser Lokalisierungstext verweist auf sich selbst",
    ),
    (
        "CW260",
        "Loc-Befehl {} im falschen Scope verwendet. In {}, erwartet wurde {}",
    ),
    ("CW261", "Schlüssel {} vom Typ {} ist mehrfach definiert"),
    (
        "CW266",
        "Lokalisierungsschlüssel {} nutzt den Befehl {}, den es im Datentyp {} nicht gibt.",
    ),
    ("CW267", "Erwartet wurde ein Wert vom Typ {}, gefunden {}"),
    (
        "CW268",
        "Lokalisierungsschlüssel {} beginnt und endet nicht mit doppelten Anführungszeichen",
    ),
    (
        "CW269",
        "Optimierung: mit {} zusammenfassen, indem du {} verwendest",
    ),
    (
        "CW270",
        "Wert zu klein; hier werden nur 3 Nachkommastellen unterstützt",
    ),
    ("CW271", "Erwartet wurde eine ganze Zahl"),
    (
        "CW273",
        "Modifier-Typ {} wird verwendet, ist aber nicht definiert",
    ),
    (
        "CW275",
        "Der Lokalisierungswert für {} enthält unerwartete Zeichen und wird eventuell falsch dargestellt",
    ),
    (
        "CW276",
        "Lokalisierungsschlüssel {} enthält ungültige Zeichen (Leerzeichen und Sonderzeichen sind nicht erlaubt)",
    ),
    (
        "CW277",
        "Die Prüfung wurde beim Alias-Verzweigungslimit abgebrochen",
    ),
    (
        "CW280",
        "{} = { always = ... } entspricht dem Standard und kann weg",
    ),
    ("CW281", "Dieses 'limit' enthält keine Trigger"),
    (
        "CW282",
        "Das ist der Standardwert ({}) und kann weggelassen werden",
    ),
    ("CW500", "Typ '{}' nicht gefunden"),
    ("CW600", "Regeldatei konnte nicht gelesen werden: {}"),
    ("CW601", "Regel verweist auf undefiniertes {} `{}`"),
];
