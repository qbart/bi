; templ's own half. Go's query goes in front of it — upstream opens this file
; `; inherits: go`, which is an instruction to the editor loading it.
;
; tree-sitter-templ ships the file and comments its HIGHLIGHTS_QUERY constant
; out, so like Julia's it is vendored, verbatim, from tree-sitter-templ 2.2.0
; (MIT). See docs/specs/tree-sitter.md.
;
; Upstream: https://github.com/vrischmann/tree-sitter-templ

; inherits: go
(component_declaration
  name: (component_identifier) @function)

[
  (tag_start)
  (tag_end)
  (self_closing_tag)
  (style_tag_start)
  (style_tag_end)
  (self_closing_style_tag)
] @tag

(attribute
  name: (attribute_name) @tag.attribute)

(attribute
  value: (quoted_attribute_value) @string)

[
  (element_text)
  (style_element_text)
] @string.special

(css_identifier) @function

(css_property
  name: (css_property_name) @property)

(css_property
  value: (css_property_value) @string)

[
  (expression)
  (dynamic_class_attribute_value)
] @function.method

(component_import
  name: (component_identifier) @function)

(component_render) @function.call

(element_comment) @comment @spell

"@" @operator

[
  "templ"
  "css"
  "script"
] @keyword
