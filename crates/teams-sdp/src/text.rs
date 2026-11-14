use std::fmt;

/// Standard SDP offer text. The receiving media engine validates it.
#[derive(Clone, PartialEq, Eq)]
pub struct SdpOffer(String);

/// Standard SDP answer text. The receiving media engine validates it.
#[derive(Clone, PartialEq, Eq)]
pub struct SdpAnswer(String);

macro_rules! sdp_text {
    ($name:ident) => {
        impl $name {
            /// Wraps standard SDP text without parsing it.
            #[must_use]
            pub fn new(text: String) -> Self {
                Self(text)
            }

            /// The SDP text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// A copy of the SDP text.
            #[must_use]
            pub fn to_sdp_string(&self) -> String {
                self.0.clone()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("media", &self.0.matches("\nm=").count())
                    .finish_non_exhaustive()
            }
        }
    };
}

sdp_text!(SdpOffer);
sdp_text!(SdpAnswer);
