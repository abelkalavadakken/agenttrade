use std::ops::{Add, Neg, Sub};

/// Fixed-point price. The scale is on the Instrument, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Price(pub i64);

/// Fixed-point quantity. The scale is on the Instrument, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Qty(pub i64);

macro_rules! scalar_ops {
    ($t:ident) => {
        impl $t {
            pub const ZERO: Self = Self(0);

            pub fn raw(self) -> i64 {
                self.0
            }

            pub fn is_zero(self) -> bool {
                self.0 == 0
            }
        }

        impl Add for $t {
            type Output = Self;
            fn add(self, rhs: Self) -> Self {
                Self(self.0 + rhs.0)
            }
        }

        impl Sub for $t {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self {
                Self(self.0 - rhs.0)
            }
        }

        impl Neg for $t {
            type Output = Self;
            fn neg(self) -> Self {
                Self(-self.0)
            }
        }
    };
}

scalar_ops!(Price);
scalar_ops!(Qty);
