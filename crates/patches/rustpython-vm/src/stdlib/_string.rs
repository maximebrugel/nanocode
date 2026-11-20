/* String builtin module
 */

pub(crate) use _string::module_def;

#[pymodule]
mod _string {
    use crate::common::ascii;
    use crate::common::format::{
        FieldName, FieldNamePart, FieldType, FormatPart, FormatString, FromTemplate,
    };
    use crate::common::wtf8::{CodePoint, Wtf8, Wtf8Buf};
    use crate::{
        PyObjectRef, PyResult, VirtualMachine,
        builtins::{PyList, PyStrRef},
        convert::ToPyException,
        convert::ToPyObject,
    };

    fn create_format_part(
        literal: Wtf8Buf,
        field_name: Option<Wtf8Buf>,
        format_spec: Option<Wtf8Buf>,
        conversion_spec: Option<CodePoint>,
        vm: &VirtualMachine,
    ) -> PyObjectRef {
        let tuple = (
            literal,
            field_name,
            format_spec,
            conversion_spec.map(|c| c.to_string()),
        );
        tuple.to_pyobject(vm)
    }

    /// Split a (valid) format string into the chunks CPython's
    /// MarkupIterator yields: each ends after an escaped brace (`{{` or
    /// `}}`, whose literal keeps one brace) or after a replacement field.
    fn markup_chunks(text: &Wtf8) -> Vec<&Wtf8> {
        let points: Vec<CodePoint> = text.code_points().collect();
        let mut offsets = Vec::with_capacity(points.len() + 1);
        let mut offset = 0;
        for c in &points {
            offsets.push(offset);
            offset += c.len_wtf8();
        }
        offsets.push(offset);
        let mut chunks = Vec::new();
        let (mut start, mut i) = (0, 0);
        while i < points.len() {
            let c = points[i];
            if (c == '{' || c == '}') && points.get(i + 1) == Some(&c) {
                i += 2;
            } else if c == '{' {
                let mut depth = 0usize;
                while i < points.len() {
                    if points[i] == '{' {
                        depth += 1;
                    } else if points[i] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                i += 1;
            } else {
                i += 1;
                continue;
            }
            let end = offsets[i.min(points.len())];
            chunks.push(&text[offsets[start]..end]);
            start = i.min(points.len());
        }
        if start < points.len() {
            chunks.push(&text[offsets[start]..]);
        }
        chunks
    }

    #[pyfunction]
    fn formatter_parser(object: PyStrRef, vm: &VirtualMachine) -> PyResult<PyList> {
        let text = object.as_wtf8();
        // Parse the whole string first so errors match str.format's.
        FormatString::from_str(text).map_err(|e| e.to_pyexception(vm))?;

        let mut result: Vec<PyObjectRef> = Vec::new();
        for chunk in markup_chunks(text) {
            let format_string =
                FormatString::from_str(chunk).map_err(|e| e.to_pyexception(vm))?;
            let mut literal = Wtf8Buf::new();
            let mut field = None;
            for part in format_string.format_parts {
                match part {
                    FormatPart::Field {
                        field_name,
                        conversion_spec,
                        format_spec,
                    } => field = Some((field_name, format_spec, conversion_spec)),
                    FormatPart::Literal(text) => literal.push_wtf8(&text),
                }
            }
            let part = match field {
                Some((name, spec, conversion)) => {
                    create_format_part(literal, Some(name), Some(spec), conversion, vm)
                }
                None if literal.is_empty() => continue,
                None => create_format_part(literal, None, None, None, vm),
            };
            result.push(part);
        }
        Ok(result.into())
    }

    #[pyfunction]
    fn formatter_field_name_split(
        object: PyStrRef,
        vm: &VirtualMachine,
    ) -> PyResult<(PyObjectRef, PyList)> {
        let field_name = FieldName::parse(object.as_wtf8()).map_err(|e| e.to_pyexception(vm))?;

        let first = match field_name.field_type {
            FieldType::Auto => vm.ctx.new_str(ascii!("")).into(),
            FieldType::Index(index) => index.to_pyobject(vm),
            FieldType::Keyword(attribute) => attribute.to_pyobject(vm),
        };

        let rest = field_name
            .parts
            .iter()
            .map(|p| match p {
                FieldNamePart::Attribute(attribute) => (true, attribute).to_pyobject(vm),
                FieldNamePart::StringIndex(index) => (false, index).to_pyobject(vm),
                FieldNamePart::Index(index) => (false, *index).to_pyobject(vm),
            })
            .collect();

        Ok((first, rest))
    }
}
