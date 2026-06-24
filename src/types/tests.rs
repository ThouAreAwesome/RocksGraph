// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//

#[cfg(test)]
mod type_tests {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    use crate::types::{
        element::{Edge, Property, Vertex},
        gvalue::{GValue, Primitive},
        keys::{CanonicalEdgeKey, CanonicalKey},
    };

    fn mock_decoder(blob: &[u8], owner: CanonicalKey) -> Option<Vec<Property>> {
        let val = if !blob.is_empty() && blob[0] == 42 { Primitive::Int32(42) } else { Primitive::Null };
        Some(vec![Property { owner, key: 10, value: val }])
    }

    #[test]
    fn test_vertex_equality_ignores_properties() {
        let v1 = Vertex::with_props(
            1,
            2,
            vec![Property { owner: CanonicalKey::Vertex(1), key: 10, value: Primitive::Int32(42) }],
        );

        let v2 = Vertex::with_props(
            1,
            2,
            vec![Property { owner: CanonicalKey::Vertex(1), key: 10, value: Primitive::String("different".into()) }],
        );

        let v3 = Vertex::from_raw(1, 2, vec![42].into_boxed_slice(), mock_decoder);

        // Equality is identity-only by design (see `PartialEq for Vertex` doc comment): v1/v2
        // share an id+label_id but disagree on the "key 10" property's value, and v3 carries no
        // decoded properties at all. All three must still compare equal.
        assert_eq!(v1, v2);
        assert_eq!(v1, v3);
        assert_eq!(v2, v3);
    }

    #[test]
    fn test_edge_equality_ignores_properties() {
        let cek = CanonicalEdgeKey { src_id: 1, label_id: 2, dst_id: 3, rank: 0 };
        let e1 = Edge::with_props(
            1,
            2,
            3,
            0,
            vec![Property { owner: CanonicalKey::Edge(cek), key: 10, value: Primitive::Int32(42) }],
        );

        let e2 = Edge::with_props(
            1,
            2,
            3,
            0,
            vec![Property { owner: CanonicalKey::Edge(cek), key: 10, value: Primitive::String("different".into()) }],
        );

        let e3 = Edge::from_raw(1, 2, 3, 0, vec![42].into_boxed_slice(), mock_decoder);

        // Equality is identity-only by design (see `PartialEq for Edge` doc comment): e1/e2
        // share the full identity tuple but disagree on the "key 10" property's value, and e3
        // carries no decoded properties at all. All three must still compare equal.
        assert_eq!(e1, e2);
        assert_eq!(e1, e3);
        assert_eq!(e2, e3);
    }

    #[test]
    fn test_gvalue_map_deterministic_hash() {
        let m1 = GValue::Map(vec![
            (GValue::Scalar(Primitive::Int32(1)), GValue::Scalar(Primitive::Int32(2))),
            (GValue::Scalar(Primitive::Int32(3)), GValue::Scalar(Primitive::Int32(4))),
        ]);

        let m2 = GValue::Map(vec![
            (GValue::Scalar(Primitive::Int32(1)), GValue::Scalar(Primitive::Int32(2))),
            (GValue::Scalar(Primitive::Int32(3)), GValue::Scalar(Primitive::Int32(4))),
        ]);

        assert_eq!(m1, m2);

        let mut h1 = DefaultHasher::new();
        m1.hash(&mut h1);
        let hash1 = h1.finish();

        let mut h2 = DefaultHasher::new();
        m2.hash(&mut h2);
        let hash2 = h2.finish();

        // Verify deterministic hash output
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_label_methods() {
        use crate::types::label::Label;
        let l1 = Label::new("test");
        let l2 = Label::from("test");
        assert_eq!(l1.0, "test");
        assert_eq!(l1, l2);
    }

    #[test]
    fn test_store_error_coverage() {
        use crate::types::StoreError;
        use std::io::Error as IoError;

        let rocks_err =
            rocksdb::DB::open_for_read_only(&rocksdb::Options::default(), "/non_existent_path_xxx/xyz", false)
                .unwrap_err();

        let errs = vec![
            StoreError::NotFound,
            StoreError::Conflict,
            StoreError::LockError,
            StoreError::DuplicateVertex(42),
            StoreError::DuplicateEdge(CanonicalEdgeKey { src_id: 1, label_id: 2, dst_id: 3, rank: 0 }),
            StoreError::Tombstoned,
            StoreError::IncidentEdges,
            StoreError::ReadOnly,
            StoreError::CorruptData("test"),
            StoreError::MissingColumnFamily("cf"),
            StoreError::RocksDb(rocks_err.clone()),
            StoreError::Io(IoError::other("io error")),
            StoreError::SchemaViolation("sv".to_string()),
            StoreError::SchemaConflict("sc".to_string()),
            StoreError::SchemaExhausted("se".to_string()),
            StoreError::UnsupportedOperation("uo".to_string()),
            StoreError::TraversalError("re".to_string()),
            StoreError::UnexpectedDataType("ud".to_string()),
        ];

        for e in &errs {
            let msg = format!("{}", e);
            assert!(!msg.is_empty());
            use std::error::Error;
            let _ = e.source();
        }

        let rocks_err2 = rocks_err.clone();
        let from_rocks: StoreError = rocks_err.into();
        assert!(matches!(from_rocks, StoreError::RocksDb(_)));

        let io_err = IoError::other("err");
        let from_io: StoreError = io_err.into();
        assert!(matches!(from_io, StoreError::Io(_)));

        // Classification helpers
        assert!(StoreError::Conflict.is_retryable());
        assert!(StoreError::LockError.is_retryable());
        assert!(!StoreError::NotFound.is_retryable());

        assert!(StoreError::RocksDb(rocks_err2.clone()).is_storage_failure());
        assert!(StoreError::Io(IoError::other("io")).is_storage_failure());
        assert!(StoreError::CorruptData("x").is_storage_failure());
        assert!(StoreError::MissingColumnFamily("x").is_storage_failure());
        assert!(!StoreError::Conflict.is_storage_failure());

        assert!(StoreError::SchemaViolation("x".into()).is_schema_error());
        assert!(!StoreError::Conflict.is_schema_error());

        assert!(StoreError::TraversalError("x".into()).is_query_error());
        assert!(StoreError::UnsupportedOperation("x".into()).is_query_error());
        assert!(StoreError::UnexpectedDataType("x".into()).is_query_error());
        assert!(!StoreError::Conflict.is_query_error());

        assert_eq!(StoreError::RocksDb(rocks_err2.clone()).category(), "storage");
        assert_eq!(StoreError::Conflict.category(), "transaction");
        assert_eq!(StoreError::SchemaViolation("x".into()).category(), "schema");
        assert_eq!(StoreError::NotFound.category(), "integrity");
        assert_eq!(StoreError::TraversalError("x".into()).category(), "query");
    }

    #[test]
    fn test_primitive_conversions_and_equality() {
        let p_bool: Primitive = true.into();
        let p_i32: Primitive = 42i32.into();
        let p_i64: Primitive = 42i64.into();
        let p_u16: Primitive = 42u16.into();
        let p_f32: Primitive = 42.0f32.into();
        let p_f64: Primitive = 42.0f64.into();
        let p_str: Primitive = "hello".into();
        let p_string: Primitive = "hello".to_string().into();
        let p_smol: Primitive = smol_str::SmolStr::new("hello").into();

        assert_eq!(p_bool, Primitive::Bool(true));
        assert_eq!(p_i32, Primitive::Int32(42));
        assert_eq!(p_i64, Primitive::Int64(42));
        assert_eq!(p_u16, Primitive::UInt16(42));
        assert_eq!(p_f32, Primitive::Float32(42.0));
        assert_eq!(p_f64, Primitive::Float64(42.0));
        assert_eq!(p_str, Primitive::String("hello".into()));
        assert_eq!(p_string, Primitive::String("hello".into()));
        assert_eq!(p_smol, Primitive::String("hello".into()));

        let mut hasher = DefaultHasher::new();
        p_bool.hash(&mut hasher);
        p_i32.hash(&mut hasher);
        p_i64.hash(&mut hasher);
        p_u16.hash(&mut hasher);
        p_f32.hash(&mut hasher);
        p_f64.hash(&mut hasher);
        p_str.hash(&mut hasher);
        Primitive::Uuid(123).hash(&mut hasher);
        Primitive::Null.hash(&mut hasher);

        assert_ne!(p_bool, p_i32);
    }

    #[test]
    fn test_primitive_predicate_evaluate_numeric_width_insensitive() {
        use crate::types::gvalue::PrimitivePredicate;

        // Eq/Ne/Within/Without must match across Int32/Int64 literals, same as Gt/Lt/etc.
        // already do via partial_cmp — a caller shouldn't see different results just because
        // they wrote `1i32` instead of `1i64` (or vice versa).
        assert!(PrimitivePredicate::Eq(Primitive::Int32(7)).evaluate(&Primitive::Int64(7)));
        assert!(!PrimitivePredicate::Ne(Primitive::Int32(7)).evaluate(&Primitive::Int64(7)));
        assert!(
            PrimitivePredicate::Within(vec![Primitive::Int64(1), Primitive::Int32(2)]).evaluate(&Primitive::Int32(2))
        );
        assert!(
            !PrimitivePredicate::Without(vec![Primitive::Int64(1), Primitive::Int32(2)]).evaluate(&Primitive::Int32(2))
        );
        assert!(
            PrimitivePredicate::Without(vec![Primitive::Int64(1), Primitive::Int32(2)]).evaluate(&Primitive::Int32(3))
        );
    }

    #[test]
    fn test_gvalue_variants_equality_and_hashing() {
        use crate::types::{keys::Direction, EdgeKey};
        use smallvec::smallvec;

        let ek = EdgeKey { primary_id: 1, direction: Direction::OUT, label_id: 2, secondary_id: 3, rank: 0 };

        let g_v = GValue::Vertex(1);
        let g_e = GValue::Edge(ek);
        let g_prop =
            GValue::Property(Property { owner: CanonicalKey::Vertex(1), key: 10, value: Primitive::Int32(42) });
        let g_scalar = GValue::Scalar(Primitive::Int32(42));
        let g_list = GValue::List(vec![g_v.clone(), g_scalar.clone()]);
        let g_map = GValue::Map(vec![(g_v.clone(), g_scalar.clone())]);
        let g_path = GValue::Path(vec![(g_v.clone(), Some(smallvec!["a".into()]))]);

        let all_gvalues = [g_v, g_e, g_prop, g_scalar, g_list, g_map, g_path];

        for i in 0..all_gvalues.len() {
            for j in 0..all_gvalues.len() {
                if i == j {
                    assert_eq!(all_gvalues[i], all_gvalues[j]);
                } else {
                    assert_ne!(all_gvalues[i], all_gvalues[j]);
                }
            }
            let mut hasher = DefaultHasher::new();
            all_gvalues[i].hash(&mut hasher);
            let _ = hasher.finish();
        }
    }
}
