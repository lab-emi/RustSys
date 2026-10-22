# rustsys_cli::proto: numbers, text and tables

**PROTO-REQ-0100** Numbers are shown with fixed decimals per unit: nanoseconds 4, area 4, seconds
3, percent 1, counts as integers without grouping. `-0.0` shows as `0.0000`; a negative value
keeps its sign when it rounds to zero. A missing value shows as `—` (TSV: empty field, JSON:
`null`). Exact mode prints the shortest decimal that reads back to the same `f64`. Control
characters in names read from inputs or reports are shown escaped.

**PROTO-REQ-0110** Every number that a front end shows in a summary is a `Shown` field: label,
text, unit and source, the source being a JSON pointer into the report or a named derivation rule
(for example `derived: count × area of the cell`). The text of a report-sourced field is the
formatted value at that pointer.

**PROTO-REQ-0120** The text results of the command line (`synth`, `check synth`) and of
`rustsys show` come from the same renderers.

**PROTO-REQ-0130** A `TableModel` gives the columns (key, title, unit, alignment) and, for any
row, the value of any column in O(1) without allocation. Models exist for synthesis cells, unused
cells and phases, and for checker mismatches.

**PROTO-REQ-0131** Filter expressions (`rustsys show --filter`): terms separated by spaces, all of
which must hold. A term is `KEY OP NUMBER` with `OP` in `<`, `<=`, `>`, `>=`, `=`; `KEY=WORD`; or a
`WORD` matched against the name column: a glob when it contains `*` or `?` (the syntax of
`--dont-use`), otherwise a substring; case-insensitive unless the word contains an uppercase letter.
An unknown key or a malformed number is an error that lists the known keys. A missing value
satisfies no numeric term.

**PROTO-REQ-0132** Sorting uses the total order of `f64` values; missing values sort last in both
directions; ties keep the model's row order, so the order does not depend on the thread count.
