# RustSys front end: accepted language and elaboration

## Inputs

**SYNTH-REQ-0101** RustSys reads one or more SystemVerilog source files, processed in the order
given. Preprocessor state (macro definitions, `default_nettype`) carries over from one file to the
next, as if the files were concatenated; each file is otherwise its own compilation unit.

**SYNTH-REQ-0102** The preprocessor implements the directives of the standard, clause 22, listed in
VERILOG-REQ-0200 to VERILOG-REQ-0205. The macro `SYNTHESIS` is predefined with an empty body unless
the user passes `--no-predefined-macros`. Further macros are defined with `-D NAME[=VALUE]`.
Include files are searched in the directory of the including file and then in each `-I` directory
in order.

**SYNTH-REQ-0103** Accepted design units: `module`, `interface` (SYNTH-REQ-0125), `package`, and
declarations in the compilation unit scope (`typedef`, `localparam`, `parameter`, `function`,
`import`). `program`, `class`, `checker`, `config`, `primitive` (user-defined primitives) and
`bind` are rejected with a diagnostic.

**SYNTH-REQ-0104** The top module is named with `--top`. Without `--top`, the top is the unique
module that no other parsed module instantiates; if there is no such module or more than one, the
run fails with a diagnostic listing the candidates.

**SYNTH-REQ-0105** Top-level parameter values may be overridden with `-P NAME=EXPR`, where `EXPR`
is a constant expression in SystemVerilog syntax evaluated in the scope of the top module's
parameter list. Overriding a `localparam` or an unknown parameter is an error.

## Declarations and types

**SYNTH-REQ-0110** Supported data types:
- integral atom and vector types: `logic`, `reg`, `bit`, `integer`, `int`, `shortint`,
  `longint`, `byte`, with optional `signed`/`unsigned`, and implicit types of nets and ports;
- packed arrays with any number of packed dimensions;
- fixed-size unpacked arrays (`[N]` and `[a:b]` forms, any number of dimensions);
- enumerations with an explicit or default (`int`) base type and explicit or implicit values;
- packed structures and packed unions, signed or unsigned, nested;
- `typedef` of any supported type, package-scoped type names (`p::t`), type parameters
  (`parameter type T = ...`), and forward-free use of types declared earlier in scope order;
- nets `wire`, `tri`, `uwire` with or without data type.

**SYNTH-REQ-0111** Rejected types (diagnostic): `real`, `shortreal`, `realtime`, `time` as a signal
type, `string` variables, `chandle`, `event`, classes, dynamic arrays, queues, associative arrays,
unpacked structures and unions, `wand`, `wor`, `triand`, `trior`, `tri0`, `tri1`, `trireg`,
`supply0`/`supply1` nets are accepted only as constant drivers of 0 and 1.
String literals are accepted wherever an integral value is expected (8 bits per character,
first character most significant), and parameters may hold such values.

**SYNTH-REQ-0112** A variable declaration with an initializer (`logic [3:0] c = 4'd5;`) acts as a
constant driver of the variable when no other statement or process drives it. When the variable is
also driven elsewhere, the initializer is ignored and a warning is issued. A net declaration
assignment (`wire w = a & b;`) is a continuous assignment.

## Module items

**SYNTH-REQ-0120** Supported module items: port declarations in ANSI and non-ANSI style;
`parameter`, `localparam` and type parameters in the parameter port list and in the body;
net and variable declarations; `typedef`; `import` of package items (explicit and wildcard);
continuous assignments; `always`, `always_comb`, `always_ff`, `always_latch`;
module instances; gate primitives `and`, `nand`, `or`, `nor`, `xor`, `xnor`, `not`, `buf`;
`generate`/`endgenerate` regions; loop, conditional and case generate constructs with optional
labels; `genvar`; `function` and `task` declarations.

**SYNTH-REQ-0121** Ignored module items (a note is issued once per kind): `initial` and `final`
procedures, immediate and concurrent assertions, `assume`, `cover`, `property`, `sequence`,
`specify` blocks, `timeunit`/`timeprecision`, attribute instances `(* ... *)`, delay values, and
`default clocking`/`default disable iff` declarations.

**SYNTH-REQ-0122** Module instances accept named, ordered, implicit-name (`.p`) and wildcard (`.*`)
port connections and named or ordered parameter overrides. Unconnected inputs read as 0 (SYNTH-
REQ-0213); unconnected outputs are left open. Arrays of instances are rejected in this version.
`defparam` is rejected.

**SYNTH-REQ-0123** Generate constructs are unrolled during elaboration. Names declared inside a
generate block are scoped by the block label (or by `genblk<n>` numbering per the standard,
clause 27.6, for unnamed blocks), and loop iterations append `[<i>]` with the genvar value.

**SYNTH-REQ-0124** Elaboration-time system tasks `$error`, `$fatal` reached during generate
elaboration fail the run with the given message; `$warning` and `$info` issue diagnostics.

**SYNTH-REQ-0125** Interfaces (IEEE 1800-2017 clause 25). An interface may contain everything a
module may contain (SYNTH-REQ-0120) plus `modport` declarations; its ports may only be inputs.
- An interface instance (`bus_if #(.W(8)) u_bus (.clk(clk));`) is elaborated like a module
  instance; its signals and state belong to the instance path (`u_bus.valid`). Interface instances
  of a scope are elaborated before the other items of that scope, so they may be connected before
  their declaration.
- A module port of interface type is written `intf_name port`, `intf_name.modport port`, or
  `interface[.modport] port` (generic). It is connected, by name, position, `.port` or `.*`, to an
  interface instance or to an interface port of the parent, optionally with a modport
  (`.p(u_bus.master)`); the interface names must match unless the port is generic, and a modport
  given both in the port and in the connection must be the same.
- `port.item` in the module denotes signal `item` of the connected instance; reads and writes act
  on that signal. When a modport applies, only the items it lists are accessible, and `input`
  items cannot be written.
- Interface ports of the top module, arrays of interface instances, subroutine calls through an
  interface port, and modport expressions are rejected with a diagnostic.

## Statements and expressions

**SYNTH-REQ-0130** Supported statements: blocking and nonblocking assignment, compound assignment
operators, increment and decrement, `if`/`else` with `unique`, `unique0` or `priority`
qualifiers, `case`/`casez`/`casex` and `case ... inside` with qualifiers, `for` (with loop
variable declarations and several initializers and steps), `while`, `do ... while`, `repeat`,
`foreach` over fixed-size arrays, `break`, `continue`, `return`, sequential blocks with local
declarations, event controls at the start of an `always` procedure, calls of functions, tasks
and void functions. Delay controls are ignored. System tasks other than those in SYNTH-REQ-0124
(for example `$display`) are ignored.

**SYNTH-REQ-0131** Supported expressions: all integral operators of the standard, clause 11,
including `inside`, `==?`/`!=?`, `**` (under SYNTH-REQ-0205), the conditional operator,
concatenation and replication (also as assignment targets), bit-selects, part-selects and
indexed part-selects with constant or variable positions, structure member selection, array
element selection, casts (`T'(e)`, `N'(e)`, `signed'(e)`, `unsigned'(e)`), assignment patterns
(positional, member-keyed, index-keyed, type-keyed and `default`), unbased unsized literals
(`'0`, `'1`, `'x`, `'z`), enumeration constants, and the system functions `$clog2`, `$bits`,
`$signed`, `$unsigned`, `$size`, `$left`, `$right`, `$high`, `$low`, `$countones`, `$onehot`,
`$onehot0`, `$isunknown`.

**SYNTH-REQ-0132** Functions are expanded at every call site. Recursive calls are accepted when
recursion ends within a depth of 256 because arguments are constant.

**SYNTH-REQ-0133** Procedural statements that change when or whether other statements run and
that the reader keeps without their operands are rejected, each with error `S0235` at the file,
line and column of the statement: `wait (cond) stmt` and `wait_order` (the reader does not keep
the condition or the statement, so ignoring them would also drop the statement), `disable`, and
the event triggers `->` and `->>`. Immediate assertions inside procedures remain ignored with a
note (SYNTH-REQ-0121); delay controls and system tasks remain ignored as SYNTH-REQ-0130 states.

## Hierarchy

**SYNTH-REQ-0140** The design is flattened. A hierarchical path is the sequence of instance names
from the top, with generate block names and loop indices inserted per SYNTH-REQ-0123, joined by
`.` (for example `u_core.gen_lane[2].u_add`).

**SYNTH-REQ-0141** Every rejected construct produces an error diagnostic with file, line and column.
No construct outside SYNTH-REQ-0121 is ignored silently.
