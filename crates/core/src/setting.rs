use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

/// A key of the settings table together with the type stored under it, so a value is always
/// read back as the type it was written with.
pub struct Setting<T> {
    key: Cow<'static, str>,
    _value: PhantomData<fn() -> T>,
}

impl<T> Setting<T> {
    pub const fn new(key: &'static str) -> Self {
        Self {
            key: Cow::Borrowed(key),
            _value: PhantomData,
        }
    }

    /// `<prefix>:<id>`, for per-entity settings (`chat:<workspace id>`).
    pub fn scoped(prefix: &str, id: impl fmt::Display) -> Self {
        Self {
            key: Cow::Owned(format!("{prefix}:{id}")),
            _value: PhantomData,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Setting<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Setting").field(&self.key).finish()
    }
}

impl<T> PartialEq for Setting<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<T> Eq for Setting<T> {}

#[cfg(test)]
mod tests {
    use super::Setting;

    #[test]
    fn keys() {
        const LAYOUT: Setting<u32> = Setting::new("layout");
        assert_eq!(LAYOUT.key(), "layout");
        assert_eq!(Setting::<String>::scoped("chat", 7).key(), "chat:7");
        assert_eq!(LAYOUT.clone(), Setting::new("layout"));
    }
}
