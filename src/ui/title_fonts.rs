use std::borrow::Cow;

const SERIF_FAMILIES: &str = "Times New Roman, Georgia, DejaVu Serif, Liberation Serif, Noto Serif";

pub(crate) fn family_stack(family: &str) -> Cow<'_, str> {
    if family
        .split(',')
        .any(|name| name.trim().eq_ignore_ascii_case("serif"))
    {
        family
            .split(',')
            .map(|name| {
                if name.trim().eq_ignore_ascii_case("serif") {
                    SERIF_FAMILIES
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(",")
            .into()
    } else {
        family.into()
    }
}
