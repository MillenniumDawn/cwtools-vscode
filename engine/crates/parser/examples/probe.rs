use cwtools_parser::ast::*;
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

fn dump(label: &str, input: &str) {
    let table = StringTable::new();
    let r = parse_string(input, &table);
    println!("== {label}: {input:?}");
    println!(
        "   roots={} errors={:?}",
        r.root_children.len(),
        r.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
    );
    fn walk(children: &[Child], arena: &Arena, t: &StringTable, depth: usize) {
        for c in children {
            match c {
                Child::Leaf(i) => {
                    let l = &arena.leaves[*i as usize];
                    let k = t.get_string(l.key.normal).unwrap_or_default();
                    match &l.value {
                        Value::Clause(ch) => {
                            println!("{:indent$}leaf {k} = clause", "", indent = depth * 2);
                            walk(ch, arena, t, depth + 1);
                        }
                        Value::String(s) | Value::QString(s) => println!(
                            "{:indent$}leaf {k} = str {:?}",
                            "",
                            t.get_string(s.normal).unwrap_or_default(),
                            indent = depth * 2
                        ),
                        v => println!("{:indent$}leaf {k} = {v:?}", "", indent = depth * 2),
                    }
                }
                Child::LeafValue(i) => {
                    let lv = &arena.leaf_values[*i as usize];
                    match &lv.value {
                        Value::String(s) | Value::QString(s) => println!(
                            "{:indent$}lv str {:?}",
                            "",
                            t.get_string(s.normal).unwrap_or_default(),
                            indent = depth * 2
                        ),
                        v => println!("{:indent$}lv {v:?}", "", indent = depth * 2),
                    }
                }
                Child::Comment(_) => println!("{:indent$}comment", "", indent = depth * 2),
            }
        }
    }
    walk(&r.root_children, &r.arena, &table, 1);
}

fn main() {
    dump(
        "quoted-then-bare",
        "x = { \"file.wav\" volume = 1 }\nnext = 2",
    );
    dump("quoted-then-number", "x = { \"name\" 1 }\ny = 3");
    dump("unclosed-rhs-quote", "a = \"oops\nb = 1\nc = 2");
    dump("glob", "");
}
