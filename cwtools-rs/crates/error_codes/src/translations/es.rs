//! Spanish diagnostic templates. Sorted by code id.

pub(crate) static TEMPLATES: &[(&str, &str)] = &[
    ("CW001", "Error al analizar el archivo de localización: {}"),
    (
        "CW100",
        "La clave de localización {} no está definida para {}",
    ),
    (
        "CW104",
        "El trigger {} se usa en un scope incorrecto. Está en {} pero se esperaba {}",
    ),
    (
        "CW105",
        "El efecto {} se usa en un scope incorrecto. Está en {} pero se esperaba {}",
    ),
    (
        "CW106",
        "El comando de scope {} se usa en un scope incorrecto. Está en {} pero se esperaba {}",
    ),
    (
        "CW107",
        "A este evento le falta mean_time_to_happen, is_triggered_only, fire_only_once o trigger={always=no}. Rendimiento: el evento puede dispararse en cada tick.",
    ),
    (
        "CW108",
        "A este research_leader le falta el \"area\" obligatorio",
    ),
    (
        "CW109",
        "Este research_leader usa el area {} pero la tecnología usa el area {}",
    ),
    ("CW110", "No se encontró categoría para esta tecnología"),
    (
        "CW113",
        "No se encontró el archivo {}; se distinguen mayúsculas y minúsculas",
    ),
    (
        "CW120",
        "El trigger {} puede convertirse en pretrigger (usa la acción de código para arreglarlo)",
    ),
    ("CW121", "Este 'if' no contiene ningún efecto"),
    (
        "CW122",
        "La clave de localización {} no debería ir entre comillas en uso inline; puede causar comportamientos inesperados",
    ),
    (
        "CW220",
        "{} o un evento al que llama necesita los objetivos de evento {}, pero no se establecen aquí ni en todos los caminos que llevan hasta aquí",
    ),
    (
        "CW221",
        "{} o un evento al que llama necesita los objetivos de evento {}, pero puede que no siempre se establezcan aquí ni en todos los caminos que llevan hasta aquí",
    ),
    ("CW222", "El id de evento {} no está definido"),
    (
        "CW223",
        "No uses NOT con varios hijos; sustitúyelo por NOR o NAND para evitar ambigüedad",
    ),
    (
        "CW223.hoi4",
        "NOT con varios hijos actúa como NOR (verdadero solo si todos los hijos son falsos). Deja clara la intención: NOT = { OR = { ... } } para NOR, o NOT = { AND = { ... } } para NAND.",
    ),
    (
        "CW225",
        "La clave de localización \"{}\" referencia \"{}\", que no existe en {}",
    ),
    (
        "CW226",
        "La clave de localización \"{}\" usa el comando \"{}\", que no existe",
    ),
    ("CW227", "No se encuentra la plantilla de sección {}"),
    ("CW228", "La plantilla de sección {} no tiene la ranura {}"),
    ("CW229", "No se encuentra la plantilla de componente {}"),
    (
        "CW230",
        "El componente y la ranura no encajan: la ranura {} tiene tamaño {} y el componente {} tiene tamaño {}",
    ),
    ("CW231", "La tecnología {} no se usa"),
    ("CW233", "La entidad {} no está definida"),
    (
        "CW234",
        "La clave de localización {} es solo un marcador de posición para {}",
    ),
    (
        "CW235",
        "El modificador {} vale 0. Los modificadores son aditivos, así que probablemente no hace nada",
    ),
    (
        "CW236",
        "El if/else anidado en efectos quedó obsoleto en la 2.1 y se eliminará en una versión futura",
    ),
    (
        "CW237",
        "La 2.1 cambió el comportamiento del if = { if else } anidado en efectos. Comprueba que sigue funcionando como esperas",
    ),
    ("CW238", "A este else/else_if le falta un if antes"),
    (
        "CW239",
        "{} de tipo {} no se usa en ninguna parte, aunque se espera que sí",
    ),
    (
        "CW243",
        "El objetivo \"{}\" tiene un scope incorrecto. Es {} pero se espera {}",
    ),
    (
        "CW244",
        "{} no es un objetivo. Se esperaba un objetivo en el scope o scopes {}",
    ),
    (
        "CW245",
        "Error en el objetivo. El enlace {} se usó en el scope {} pero se esperaba {}",
    ),
    ("CW246", "La variable {} nunca se ha establecido"),
    (
        "CW247",
        "El trigger/efecto/modificador {} se usa en un scope incorrecto. Está en {} pero se espera {}",
    ),
    ("CW248", "Comando de scope {} no válido"),
    ("CW251", "Este {} sobra"),
    ("CW253", "Usa mejor \"set_name\" por coherencia"),
    (
        "CW254",
        "Los archivos de localización deben ser UTF-8 con BOM; este no lo es",
    ),
    (
        "CW255",
        "El nombre del archivo de localización debería contener \"l_language.yml\", idealmente al final",
    ),
    (
        "CW256",
        "Un archivo de localización debería empezar por \"l_language:\" en la primera línea (o por un comentario)",
    ),
    (
        "CW257",
        "El nombre del archivo de localización indica el idioma {}, que no coincide con el idioma {} de la cabecera",
    ),
    (
        "CW259",
        "Esta cadena de localización se referencia a sí misma",
    ),
    (
        "CW260",
        "El comando loc {} se usa en un scope incorrecto. Está en {} pero se esperaba {}",
    ),
    ("CW261", "La clave {} de tipo {} está definida varias veces"),
    (
        "CW266",
        "La clave de localización {} usa el comando {}, que no existe en el tipo de datos {}.",
    ),
    ("CW267", "Se esperaba un valor de tipo {} y llegó {}"),
    (
        "CW268",
        "La clave de localización {} no empieza ni acaba con comillas dobles",
    ),
    ("CW269", "Optimiza fusionando esto con {} usando {}"),
    (
        "CW270",
        "Valor demasiado pequeño: aquí solo se admiten 3 decimales",
    ),
    ("CW271", "Se esperaba un número entero"),
    (
        "CW273",
        "El tipo de modificador {} se usa pero no está definido",
    ),
    (
        "CW275",
        "El valor de localización de {} tiene caracteres inesperados y puede no mostrarse bien",
    ),
    (
        "CW276",
        "La clave de localización {} tiene caracteres no válidos (no se permiten espacios ni caracteres especiales)",
    ),
    (
        "CW277",
        "La validación se detuvo al llegar al límite de ramas de alias",
    ),
    (
        "CW280",
        "{} = { always = ... } coincide con el valor predeterminado y se puede quitar",
    ),
    ("CW281", "Este 'limit' no contiene ningún trigger"),
    (
        "CW282",
        "Este es el valor predeterminado ({}) y se puede omitir",
    ),
    ("CW500", "No se encontró el tipo '{}'"),
    ("CW600", "No se pudo leer el archivo de reglas: {}"),
    ("CW601", "La regla referencia un {} `{}` no definido"),
];
