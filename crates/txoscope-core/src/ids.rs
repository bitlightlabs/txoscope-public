//! Opaque domain identities. Values are preserved without validation or normalization.
use core::fmt;

macro_rules! identifier {
	($name:ident, $description:literal) => {
		#[doc = $description]
		#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
		pub struct $name(String);

		impl $name {
			/// Preserves the opaque value without validation or normalization.
			pub fn new(value: impl Into<String>) -> Self {
				Self(value.into())
			}
			/// Borrows the original identifier value.
			pub fn as_str(&self) -> &str {
				&self.0
			}
			/// Consumes the identifier and returns its original value.
			pub fn into_string(self) -> String {
				self.0
			}
		}
		impl From<String> for $name {
			fn from(value: String) -> Self {
				Self(value)
			}
		}
		impl From<&str> for $name {
			fn from(value: &str) -> Self {
				Self(value.to_owned())
			}
		}
		impl AsRef<str> for $name {
			fn as_ref(&self) -> &str {
				self.as_str()
			}
		}
		impl fmt::Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.write_str(self.as_str())
			}
		}
	};
}

identifier!(AccountId, "An opaque account identity.");
identifier!(WalletScopeId, "An opaque wallet scope identity.");
identifier!(OperationId, "An opaque operation identity, including manual reservations.");

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn identifiers_preserve_opaque_values() {
		for value in ["", "账户:scope / operation", "  punctuation!?  "] {
			assert_eq!(AccountId::new(value).into_string(), value);
			assert_eq!(WalletScopeId::from(value).as_str(), value);
			assert_eq!(OperationId::from(value.to_owned()).to_string(), value);
		}
	}
}
