//! Handler descriptors for the manifest layer.
//!
//! `HandlerDescriptor` describes a single request handler — its route,
//! service, input/output schemas, middleware chain, etc. This is the
//! primary manifest entry produced by the TS build pipeline and consumed
//! by the Rust gateway at startup.
//!
//! **Phase 1 status:** placeholder. The real descriptor fields land in
//! Phase 2 when the manifest emitter is built. The type exists now so
//! that dependent crates can reference it without awaiting Phase 2.

/// Describes a single handler in the manifest.
#[derive(Debug, Clone)]
pub struct HandlerDescriptor {
    // Private field prevents external construction via struct literal
    // syntax, so adding fields later is non-breaking. See the
    // `placeholder()` constructor for Phase 1 usage.
    _private: (),
}

impl HandlerDescriptor {
    /// Construct a placeholder descriptor for Phase 1 demo purposes.
    /// Real construction arrives with the manifest parser in Phase 2.
    #[must_use]
    pub fn placeholder() -> Self {
        Self { _private: () }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_constructs() {
        let _d = HandlerDescriptor::placeholder();
    }

    #[test]
    fn descriptor_is_clonable() {
        let a = HandlerDescriptor::placeholder();
        let _b = a.clone();
    }
}
