# Issue 512: give unquestion.question a schema type

`unquestion`'s `question` parameter (`QuestionRef`, an untagged enum of integer
and string) was published as an `anyOf` with no `type`, which the Claude
connector directory review flags. The property now also carries
`"type": ["integer", "string"]` (a `#[schemars(extend(...))]` on the field).
The `$ref` to `QuestionRef` and the description are unchanged, and deserialization is
untouched: both a number and a string still parse.

The ("unquestion", "question") entry is removed from
`KNOWN_UNTYPED_PARAMETERS`. A new test checks the published type and that both
forms still deserialize.
