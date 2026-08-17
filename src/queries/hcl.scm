; Highlights for HCL / Terraform.
;
; The one query bi ships itself. `tree-sitter-hcl` publishes a parser and no
; queries at all — the upstream repository has them, but crates.io excludes
; them from the package and `include_str!` cannot reach into a dependency, so
; the alternative was Terraform parsing correctly and rendering entirely
; plain. Written against `src/node-types.json` of that crate.

(comment) @comment

(bool_lit) @boolean
(null_lit) @constant.builtin
(numeric_lit) @number

(string_lit) @string
(heredoc_template) @string
(template_literal) @string
(heredoc_identifier) @label

; `resource "aws_instance" "web" { … }` — the block type first, then its
; labels. The label pattern comes after the general `(string_lit) @string`
; deliberately: both capture the identical node, and the later one wins.
(block (identifier) @keyword)
(block (string_lit (template_literal) @type))

; `ami = "…"` — the name on the left of an attribute, and of an object entry.
(attribute (identifier) @property)
(object_elem key: (expression) @property)

(function_call (identifier) @function)

; `var.region` — the root is a variable, and every `.name` hanging off it
; reads as a field.
(variable_expr (identifier) @variable)
(get_attr (identifier) @property)

; The `${` and `%{` that open an interpolation, and their closers.
[
  (template_interpolation_start)
  (template_interpolation_end)
  (template_directive_start)
  (template_directive_end)
] @punctuation.special

(ellipsis) @punctuation.special
