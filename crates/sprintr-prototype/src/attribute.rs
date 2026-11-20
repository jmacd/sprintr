// SPDX-License-Identifier: Apache-2.0

/// An immutable string attribute declared with [`crate::attribute!`].
///
/// Use a `static` or `const` declaration so logging macros can pre-encode the
/// entire statement template, including its canonical attribute names.
pub struct Attribute {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) encoded_definition: &'static [u8],
    format_name: &'static [u8],
    reference_prefix: &'static [u8],
}

impl Attribute {
    #[doc(hidden)]
    pub const fn __new(
        name: &'static str,
        description: &'static str,
        encoded_definition: &'static [u8],
        format_name: &'static [u8],
        reference_prefix: &'static [u8],
    ) -> Self {
        Self {
            name,
            description,
            encoded_definition,
            format_name,
            reference_prefix,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[doc(hidden)]
    pub const fn __format_name(&self) -> &'static [u8] {
        self.format_name
    }

    #[doc(hidden)]
    pub const fn __reference_prefix(&self) -> &'static [u8] {
        self.reference_prefix
    }

    #[doc(hidden)]
    pub fn __value<'a, V: AsRef<str> + ?Sized>(&self, value: &'a V) -> &'a str {
        value.as_ref()
    }
}
