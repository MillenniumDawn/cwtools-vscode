//! Arabic. Sorted by key id.

pub(crate) static UI: &[(&str, &str)] = &[
    ("action.createLocKey", "إنشاء مفتاح الترجمة {}"),
    ("action.didYouMean", "هل تقصد '{}'؟"),
    ("action.fixAll", "إصلاح الكل ({} قابل للإصلاح تلقائيًا)"),
    ("action.ignoreCode", "تجاهل {} في مساحة العمل هذه"),
    ("action.removeEmptyIf", "حذف كتلة if الفارغة"),
    ("action.removeEmptyLimit", "حذف كتلة limit الفارغة"),
    ("action.removeRedundant", "حذف {} الزائد"),
    (
        "action.removeRedundantDefault",
        "حذف القيمة الافتراضية الزائدة",
    ),
    (
        "action.removeUnnecessaryQuotes",
        "حذف علامات الاقتباس غير اللازمة",
    ),
    ("action.renameToSetName", "إعادة التسمية إلى set_name"),
    (
        "command.cachesCleared",
        "مُسحت الذواكر المؤقتة ({} ملفًا)؛ {}.",
    ),
    (
        "command.cachesClearedWithErrors",
        "مُسحت الذواكر المؤقتة ({} ملفًا) مع {} خطأ؛ {}. فشل: {}",
    ),
    (
        "command.noRulesDirectory",
        "لا يوجد مجلد قواعد مُعدّ؛ لا شيء لإعادة تحميله.",
    ),
    ("command.noRulesLoaded", "لم تُحمَّل أي قواعد من {}؛ {}."),
    ("command.reindexCancelled", "أُلغيت إعادة الفهرسة."),
    ("command.reindexInProgress", "إعادة الفهرسة جارية بالفعل."),
    ("command.rulesReloaded", "أُعيد تحميل إعدادات القواعد؛ {}."),
    ("command.workspaceReindexed", "أُعيدت فهرسة مساحة العمل."),
    ("hover.description", "الوصف"),
    ("hover.localisation", "الترجمة"),
    ("hover.requiredScopes", "النطاقات المطلوبة"),
    ("hover.resolvesTo", "يُحَل إلى"),
    ("hover.scope", "النطاق"),
    ("progress.cancelled", "أُلغي."),
    ("progress.discover", "يجري فحص مساحة العمل…"),
    ("progress.localisation", "يجري بناء فهرس الترجمة…"),
    ("progress.parse", "يجري فهرسة مساحة العمل…"),
    ("progress.publish", "يجري نشر التشخيصات…"),
    ("progress.validate", "يجري التحقق من مساحة العمل…"),
    ("progress.vanilla", "يجري فهرسة اللعبة الأساسية…"),
    (
        "status.reindexCancelledRebuilding",
        "أُلغيت إعادة الفهرسة، وتجري إعادة البناء في الخلفية",
    ),
    (
        "status.reindexPending",
        "إعادة الفهرسة لا تزال معلّقة (هناك فحص آخر جارٍ)",
    ),
    ("status.reindexed", "أُعيدت فهرسة مساحة العمل"),
    ("status.revalidated", "أُعيد التحقق من مساحة العمل"),
    ("status.revalidationCancelled", "أُلغيت إعادة التحقق"),
    (
        "status.revalidationPending",
        "إعادة التحقق لا تزال معلّقة (هناك فحص جارٍ)",
    ),
    (
        "status.revalidationQueued",
        "أُدرجت إعادة التحقق بعد الفحص الجاري",
    ),
];
