use anyhow::{Context, Result};
use std::collections::HashMap;

// A parsed version of a package METADATA or PKG-INFO or WHEEL file, as per
// https://packaging.python.org/specifications/core-metadata/
pub type Fields = HashMap<String, Vec<String>>;

#[cfg(test)]
use serde::Deserialize;

#[cfg_attr(test, derive(Debug, Deserialize, PartialEq))]
pub struct RFC822ish {
    pub fields: Fields,
    pub body: Option<String>,
}

mod parser_internals {
    use super::*;
    use nom::bytes::complete::{is_a, is_not, take_while1};
    use nom::character::complete::one_of;
    use nom::combinator::rest;
    use nom::multi::many1;
    use nom::sequence::separated_pair;
    use nom::{IResult, Parser};
    use nom_supreme::{
        error::ErrorTree,
        final_parser::{final_parser, Location},
        parser_ext::ParserExt,
        tag::complete::tag,
    };

    type ParseResult<'a, O> = IResult<&'a str, O, ErrorTree<&'a str>>;

    // Allegedly, a METADATA file is formatted as an RFC822 email message.
    // This is absolutely not true. The actual format is "whatever
    // the Python stdlib module email.message_from_string does".
    //
    // Overall structure: A series of header lines, then an empty line, then
    // the "message body" (= package readme)
    //
    // A line ending is: \n, \r, or \r\n
    //
    // A header line is:
    // - field name + separator + field value + line ending
    // field names are:
    // - a sequence of one or more characters in the set [\041-\071\073-\176]
    //   or put another way: anything from \041-\176 except for ':'
    // The field name/value separator is:
    // - a colon + zero or more spaces or tabs
    // The field value is:
    // - everything after the separator, until the end of the line, not
    //   including the end of line.
    //   BUT we keep reading if the line ending is followed by a space or tab!
    //   So e.g.:
    //
    //     "foo: \tbar  \n  baz\r\n"
    //
    //   ...will produce the field value:
    //
    //     "bar  \n  baz"
    //
    // Some notable differences from RFC 822:
    // - continuation lines preserve newlines; RFC822 says that they should be
    //   replaced by spaces.
    // - RFC822 says that \r\n is mandatory at the end of all lines
    // - RFC822's definitions of whitespace are a bit different
    //
    // The 'email' module is also extremely lenient of errors. We'll try to be
    // a bit more strict -- we try to be lenient of mangled utf-8, because
    // obviously someone must have messed that up in the history of PyPI, but
    // we fail on oddities like an empty field name or a continuation line at
    // the start of input.

    peg::parser! {
        grammar rfc822ish_parser() for str {
            rule line_ending()
                = quiet!{"\r\n" / "\r" / "\n"}
                  / expected!("end of line")

            rule field_name() -> &'input str
                = quiet!{$(['\x21'..='\x39' | '\x3b'..='\x7e']+)}
                  / expected!("field name")

            rule field_separator()
                = ":" [' ' | '\t']*

            rule field_value_piece()
                = [^ '\r' | '\n']*

            rule continuation_line_ending()
                = line_ending() [' ' | '\t']

            rule field_value() -> &'input str
                = $(field_value_piece() ** continuation_line_ending())

            rule field() -> (String, String)
                = n:field_name() field_separator() v:field_value()
                    { (n.to_owned(), v.to_owned()) }

            rule fields() -> Vec<(String, String)>
                = field() ** line_ending()

            rule trailing_body() -> String
                = line_ending() line_ending() b:$([_]*) { b.to_owned() }

            rule rfc822ish() -> RFC822ish
                = f:fields() body:(trailing_body()?)
                     {
                         let mut fields = Fields::new();
                         for (name, value) in f {
                             fields.entry(name).or_insert(Vec::new()).push(value)
                         };
                         RFC822ish { fields, body, }
                     }
        }
    }

    pub fn parse_metadata(input: &str) -> Result<RFC822ish, ErrorTree<Location>> {
        // This has to be an actual function, not just a combinator object,
        // because nom's type system is awkward and if it were a combinator
        // there would be no way to use it multiple times below, even as
        // borrows.
        fn line_ending(input: &str) -> ParseResult<()> {
            tag("\r\n")
                .or(tag("\r"))
                .or(tag("\n"))
                .map(|_| ())
                .parse(input)
        }

        fn is_field_name_char(c: char) -> bool {
            let i = c as u32;
            0o41 <= i && i <= 0o176 && c != ':'
        }

        let field_name = take_while1(is_field_name_char).context("field name");
        let field_separator = tag(":").and(is_a(" \t")).context("field separator");

        let value_line_piece = is_not("\r\n");
        let continuation_marker = line_ending.and(one_of(" \t"));
        let field_value =
            nom::multi::separated_list1(continuation_marker, value_line_piece)
                .recognize()
                .context("field value");

        let field =
            separated_pair(field_name, field_separator, field_value).context("field");
        let fields = many1(field.terminated(line_ending)).context("fields");

        let body = rest.preceded_by(line_ending);

        let metadata = nom::sequence::pair(fields, body.opt());
        let mut parse = final_parser(metadata);
        let (fields_vec, body) = parse(input)?;
        let mut fields = Fields::new();
        for (field_name, field_value) in fields_vec {
            fields
                .entry(field_name.to_owned())
                .or_insert(Vec::new())
                .push(field_value.to_owned());
        }
        // Convert from Option<&str> to Option<String>
        let body = body.map(String::from);
        Ok(RFC822ish { fields, body })
    }
}

impl RFC822ish {
    pub fn parse(data: &str) -> Result<RFC822ish> {
        parser_internals::parse_metadata(data).context("Error parsing metadata")
    }
}

pub struct CoreMetadata(Fields);

impl CoreMetadata {
    pub fn parse(data: &str) -> Result<CoreMetadata> {
        let mut rfc822ish = RFC822ish::parse(data)?;
        if let Some(body) = rfc822ish.body {
            rfc822ish
                .fields
                .entry("Description".to_string())
                .or_insert(Vec::new())
                .push(body);
        }
        Ok(CoreMetadata(rfc822ish.fields))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use indoc::indoc;

    #[test]
    fn test_successful_parsing() {
        struct T {
            // Input to parser
            given: &'static str,
            // Expected parsed data structure, written as json
            expected: &'static str,
        }

        let test_cases = vec![
            T {
                given: indoc! {r#"
                   A: b
                   C: d
                      continued

                   this is the
                   body!
                "#},
                expected: indoc! {r#"
                   {
                     "fields": { "A": ["b"], "C": ["d\n   continued"]},
                     "body": "this is the\nbody!\n"
                   }
                "#},
            },
            T {
                given: indoc! {r#"
                   no: body
                "#},
                expected: indoc! {r#"
                   {"fields": {"no": ["body"]}}
                "#},
            },
            T {
                given: indoc! {r#"
                   duplicate: one
                   duplicate: two
                   another: field
                   duplicate: three
                "#},
                expected: indoc! {r#"
                   {"fields": {"duplicate": ["one", "two", "three"], "another": ["field"]}}
                "#},
            },
            T {
                given: indoc! {r#"
                   no: trailing newline"#},
                expected: indoc! {r#"
                   {"fields": {"no": ["trailing newline"]}}
                "#},
            },
        ];

        for test_case in test_cases {
            let got = RFC822ish::parse(test_case.given).unwrap();
            let expected: RFC822ish = serde_json::from_str(test_case.expected).unwrap();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn test_failed_parsing() {
        let test_cases = vec![
            "",
            indoc! {r#"
                  continuation line
               at: beginning

               not good
            "#},
            indoc! {r#"
               bad key name: whee
            "#},
            ": no key name\n",
        ];
        for test_case in test_cases {
            let got = RFC822ish::parse(test_case);
            println!("{:?} -> {:?}", test_case, got);
            assert!(got.is_err());
        }
    }
}
