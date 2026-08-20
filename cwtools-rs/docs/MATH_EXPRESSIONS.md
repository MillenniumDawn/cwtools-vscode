# Math Expressions

HOI4 lets a variable be set from a *math expression*: a block that starts with a
`value` and then transforms it through a sequence of operator keys. The validator
checks these blocks structurally so a mis-typed operator is caught instead of
silently becoming a stray variable assignment.

Everything below is the vanilla engine's own behavior, taken from the game's
`documentation/` folder (`script_math_functions.md`, `effects_documentation.md`,
`triggers_documentation.md`, `script_collection_operator.md`). The feature is new,
so that folder is the authoritative reference.

## The accumulator model

A math block is an **accumulator**. The first `value` seeds it; each operator
after that reads the running accumulator and the operator's argument, and writes
the result back. Operators apply top to bottom.

```
# ((num_cats * 2) + 1)
set_variable = { num_dogs = { value = num_cats  multiply = 2  add = 1 } }
```

An operator argument is one of:

- a **number** (`multiply = 2`),
- a **variable reference** (`add = num_factories`), or
- a **nested math block** (`multiply = { value = a  add = b }`), which is itself
  an accumulator evaluated first.

## Where math expressions are accepted

Both effect forms take a math block, per `effects_documentation.md`:

```
# direct form: the variable name is the key
set_variable = { my_var = { value = base  subtract = other  multiply = 0.25 } }

# explicit form: var / value
set_variable = { var = my_var  value = { value = base  add = 1 } }
```

The same applies to `set_temp_variable` and the arithmetic family
(`add_to_variable`, `subtract_from_variable`, `multiply_variable`,
`divide_variable`) and their `*_temp_variable` variants. The value side of any of
these can be a plain number, a variable, or a math block.

Two triggers evaluate a math expression directly (`triggers_documentation.md`):

| Trigger | Result |
|---|---|
| `check_expr` | true when the expression evaluates to non-zero |
| `debug_math_expr` | writes the numeric result to the tooltip |

```
check_expr = { value = num_factories  subtract = 10  greater_than = 0 }
```

`tooltip = <loc_key>` is allowed in any math block to override the tooltip title.

## Operator reference

From `script_math_functions.md`. The argument is a number, a variable, or a
nested math block unless noted otherwise.

| Operator | Effect on the accumulator |
|---|---|
| `add` | adds the argument |
| `subtract` | subtracts the argument |
| `multiply` | multiplies by the argument |
| `divide` | divides by the argument |
| `min` | sets it to `min(accumulator, argument)` |
| `max` | sets it to `max(accumulator, argument)` |
| `clamp = { min = a  max = b }` | clamps between bounds (order sensitive; either bound may be omitted) |
| `mod` | sets it to the remainder of dividing by the argument |
| `pow` | raises it to the given power |
| `root` | takes the nth root (`2` = square root, `3` = cube root, …) |
| `log` | sets it to its logarithm in the given base |
| `sin` / `cos` / `tan` | trig of the accumulator, in radians (argument `yes`) |
| `round` | rounds to the nearest integer (argument `yes`) |
| `greater_than` / `less_than` | returns 1 if the comparison holds, else 0 |
| `greater_than_or_equals` / `less_than_or_equals` | returns 1 if the comparison holds, else 0 |
| `equals` / `not_equals` | returns 1 if the comparison holds, else 0 |
| `if` / `else` | conditional (see below) |
| `every_collection` | iterates a named collection (see below) |

`else_if` is not in the vanilla list above but appears in real mod code as the
expected middle branch between `if` and `else`; the HOI4 config declares it, so
the validator accepts it. Same for five more the config carries that the list
above doesn't:

| Operator | Effect on the accumulator |
|---|---|
| `and` / `or` | returns 1 if both / either of the accumulator and the argument are non-zero, else 0 (short-circuiting) |
| `atan` | arctangent of the accumulator, in radians (argument `yes`) |
| `atan2` | quadrant-aware `atan2(accumulator, argument)` |
| `lerp = { to = a  alpha = b }` | interpolates from the accumulator toward `to` by `alpha` |

`log`, `root`, `pow` (fractional exponents), `sin`, `cos`, and `tan` are numerical
approximations and can produce small rounding errors (e.g. `3.00001`). Follow with
`round = yes` when you need an exact integer.

### Conditionals

`if`'s `limit` is itself a math expression (true when non-zero); `else_if` and
`else` follow it. The branch bodies are more math operators.

```
my_var = {
    value = x
    if = { limit = { value = x  greater_than = 10 }  add = 100 }
    else = { subtract = 1 }
}
```

The vanilla docs describe `limit` as a math expression (true when non-zero). In
practice mods also put ordinary triggers there (e.g. `check_variable`) to key off
game state, so the validator permits both.

### Iteration

`every_collection` runs its operator body once per element of a named collection
(`faction_members`, `owned_states`, … from `script_collection_operator.md`).

```
total = {
    value = 0
    every_collection = {
        named_collection = faction_members
        add = num_researched_technologies
    }
}
```

## What the validator checks

Inside a math block the only valid keys are `value`, `tooltip`, and the operators
above. The operator set is not baked into the validator: it is whatever the
ruleset declares as `alias[mathexpr:...]`, and the argument shapes come from the
same definitions, so `else_if` and anything else the config adds is accepted
because the config says so. Anything else is flagged:

- `CW263`: unexpected leaf field (e.g. a mis-typed `subtrac = x`).
- `CW262`: unexpected block.

This is enforced **strictly**: the variable-math effects also accept a plain
`name = number` assignment, and that permissive form will not be used to excuse a
mis-typed operator inside a block. So `set_variable = { v = { value = a  multply = 2 } }`
reports `multply` rather than silently reading it as "assign a variable named
`multply`".

Completion inside a math block offers `value`, the operator keys, and the
variables defined in the project.
