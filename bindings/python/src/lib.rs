// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use rocksgraph::{
    bulk::{BulkEdge, BulkLoader, BulkVertex},
    schema::{
        AnnAlgorithm, DataType, DistanceMetric, EdgeMode, GraphOptions, HnswConfig, IndexOptions, PerIndexOptions,
        Quantization, SchemaMode, SchemaSession, VectorEntityType, VectorIndexConfig, VectorIndexLimit,
    },
    Graph, IndexManager, Primitive, ReadSession, RocksOptions, TxnSession, Value,
};
use smol_str::SmolStr;
use std::collections::HashMap;
use std::path::PathBuf;

mod errors;
use errors::store_error_to_pyerr;

/// Map a Rust gremlin::value::Value to a Python object.
fn value_to_py(py: Python<'_>, value: Value) -> PyResult<PyObject> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(b) => Ok(b.into_py(py)),
        Value::Int32(i) => Ok(i.into_py(py)),
        Value::Int64(i) => Ok(i.into_py(py)),
        Value::UInt16(i) => Ok(i.into_py(py)),
        Value::Float32(f) => Ok(f.into_py(py)),
        Value::Float64(f) => Ok(f.into_py(py)),
        Value::String(s) => Ok(s.to_string().into_py(py)),
        Value::Vertex(v) => {
            let dict = PyDict::new_bound(py);
            dict.set_item("id", v.id)?;
            dict.set_item("label", v.label.to_string())?;
            let props = PyDict::new_bound(py);
            for (k, val) in v.properties {
                props.set_item(k.to_string(), value_to_py(py, val)?)?;
            }
            dict.set_item("properties", props)?;
            Ok(dict.into())
        }
        Value::Edge(e) => {
            let dict = PyDict::new_bound(py);
            dict.set_item("src", e.out_v)?;
            dict.set_item("dst", e.in_v)?;
            dict.set_item("label", e.label.to_string())?;
            dict.set_item("rank", e.rank)?;

            let props = PyDict::new_bound(py);
            for (k, val) in e.properties {
                props.set_item(k.to_string(), value_to_py(py, val)?)?;
            }
            dict.set_item("properties", props)?;
            Ok(dict.into())
        }
        Value::Property(p) => {
            let dict = PyDict::new_bound(py);
            dict.set_item("key", p.key.to_string())?;
            dict.set_item("value", value_to_py(py, *p.value)?)?;
            Ok(dict.into())
        }
        Value::List(l) => {
            let lst = PyList::empty_bound(py);
            for item in l {
                lst.append(value_to_py(py, item)?)?;
            }
            Ok(lst.into())
        }
        Value::Map(m) => {
            let dict = PyDict::new_bound(py);
            for (k, v) in m.entries {
                let py_k = match k {
                    Value::Vertex(vx) => vx.id.into_py(py),
                    Value::Edge(eg) => (eg.out_v, eg.in_v, eg.label.to_string(), eg.rank).into_py(py),
                    other => value_to_py(py, other)?,
                };
                let py_v = value_to_py(py, v)?;
                dict.set_item(py_k, py_v)?;
            }
            Ok(dict.into())
        }
        Value::Path(p) => {
            let dict = PyDict::new_bound(py);
            let objects = PyList::empty_bound(py);
            for obj in p.objects {
                objects.append(value_to_py(py, obj)?)?;
            }
            dict.set_item("objects", objects)?;
            let labels_lst = PyList::empty_bound(py);
            for lbls in p.labels {
                let inner = PyList::empty_bound(py);
                for l in lbls {
                    inner.append(l)?;
                }
                labels_lst.append(inner)?;
            }
            dict.set_item("labels", labels_lst)?;
            Ok(dict.into())
        }
        Value::Bytes(b) => Ok(PyBytes::new_bound(py, &b).into()),
        Value::FloatVector(v) => {
            let lst = PyList::empty_bound(py);
            for f in v {
                lst.append(f.into_py(py))?;
            }
            Ok(lst.into())
        }
        Value::Uuid(u) => {
            let b = u.to_be_bytes();
            let s = format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                u32::from_be_bytes(b[0..4].try_into().unwrap()),
                u16::from_be_bytes(b[4..6].try_into().unwrap()),
                u16::from_be_bytes(b[6..8].try_into().unwrap()),
                u16::from_be_bytes(b[8..10].try_into().unwrap()),
                u64::from_be_bytes([0, 0, b[10], b[11], b[12], b[13], b[14], b[15]])
            );
            Ok(s.into_py(py))
        }
    }
}

/// Convert a list of values to a python list of objects.
fn values_to_py_list(py: Python<'_>, values: Vec<Value>) -> PyResult<PyObject> {
    let lst = PyList::empty_bound(py);
    for val in values {
        lst.append(value_to_py(py, val)?)?;
    }
    Ok(lst.into())
}

/// Convert a Python value into a rocksgraph Primitive.
fn py_to_primitive(val: &Bound<'_, PyAny>) -> PyResult<Primitive> {
    if val.is_none() {
        return Ok(Primitive::Null);
    }
    if let Ok(b) = val.downcast::<pyo3::types::PyBool>() {
        return Ok(Primitive::Bool(b.is_true()));
    }
    let type_name = val.get_type().name()?.to_string();
    if type_name == "Int32" {
        let v: i32 = val.getattr("value")?.extract()?;
        return Ok(Primitive::Int32(v));
    }
    if type_name == "Int64" {
        let v: i64 = val.getattr("value")?.extract()?;
        return Ok(Primitive::Int64(v));
    }
    if type_name == "UInt16" {
        let v: u16 = val.getattr("value")?.extract()?;
        return Ok(Primitive::UInt16(v));
    }
    if type_name == "Float32" {
        let v: f32 = val.getattr("value")?.extract()?;
        return Ok(Primitive::Float32(v));
    }
    if type_name == "Float64" {
        let v: f64 = val.getattr("value")?.extract()?;
        return Ok(Primitive::Float64(v));
    }
    if type_name == "Uuid" {
        let v: String = val.getattr("value")?.extract()?;
        let clean = v.replace('-', "");
        let u = u128::from_str_radix(&clean, 16)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid UUID string '{v}': {e}")))?;
        return Ok(Primitive::Uuid(u));
    }
    if type_name == "Vector" {
        let values: Vec<f32> = val.getattr("values")?.extract()?;
        return Ok(Primitive::FloatVector(values));
    }
    if let Ok(i) = val.extract::<i64>() {
        return Ok(Primitive::Int64(i));
    }
    if let Ok(f) = val.extract::<f64>() {
        return Ok(Primitive::Float64(f));
    }
    if let Ok(s) = val.extract::<String>() {
        return Ok(Primitive::String(SmolStr::from(s)));
    }
    if let Ok(b) = val.downcast::<pyo3::types::PyBytes>() {
        return Ok(Primitive::Bytes(b.as_bytes().to_vec()));
    }
    if let Ok(list) = val.extract::<Vec<f32>>() {
        return Ok(Primitive::FloatVector(list));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "Cannot convert Python value of type '{type_name}' to Primitive"
    )))
}

fn py_props_to_hashmap(props_dict: &Bound<'_, PyDict>) -> PyResult<HashMap<String, Primitive>> {
    let mut map = HashMap::with_capacity(props_dict.len());
    for (k, v) in props_dict.iter() {
        let key: String = k.extract()?;
        let prim = py_to_primitive(&v)?;
        map.insert(key, prim);
    }
    Ok(map)
}

fn py_to_bulk_vertex(item: &Bound<'_, PyAny>) -> PyResult<BulkVertex> {
    let (id, label, props_map) = if let Ok(dict) = item.downcast::<PyDict>() {
        let id: i64 = dict
            .get_item("id")?
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Missing 'id' in vertex"))?
            .extract()?;
        let label: String = dict
            .get_item("label")?
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Missing 'label' in vertex"))?
            .extract()?;
        let props_dict = match dict.get_item("props")?.or(dict.get_item("properties")?) {
            Some(p) => p.downcast::<PyDict>()?.clone(),
            None => PyDict::new_bound(dict.py()),
        };
        (id, label, py_props_to_hashmap(&props_dict)?)
    } else {
        let id: i64 = item.getattr("id")?.extract()?;
        let label: String = item.getattr("label")?.extract()?;
        let props_obj = item.getattr("props")?;
        let props_dict = props_obj.downcast::<PyDict>()?;
        (id, label, py_props_to_hashmap(props_dict)?)
    };

    Ok(BulkVertex { id, label, props: props_map })
}

fn py_to_bulk_edge(item: &Bound<'_, PyAny>) -> PyResult<BulkEdge> {
    let (src, dst, label, rank, props_map) = if let Ok(dict) = item.downcast::<PyDict>() {
        let src: i64 = dict
            .get_item("src")?
            .or(dict.get_item("out_v")?)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Missing 'src' in edge"))?
            .extract()?;
        let dst: i64 = dict
            .get_item("dst")?
            .or(dict.get_item("in_v")?)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Missing 'dst' in edge"))?
            .extract()?;
        let label: String = dict
            .get_item("label")?
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Missing 'label' in edge"))?
            .extract()?;
        let rank: Option<u16> = dict.get_item("rank")?.map(|r| r.extract()).transpose()?;
        let props_dict = match dict.get_item("props")?.or(dict.get_item("properties")?) {
            Some(p) => p.downcast::<PyDict>()?.clone(),
            None => PyDict::new_bound(dict.py()),
        };
        (src, dst, label, rank, py_props_to_hashmap(&props_dict)?)
    } else {
        let src: i64 = item.getattr("src")?.extract()?;
        let dst: i64 = item.getattr("dst")?.extract()?;
        let label: String = item.getattr("label")?.extract()?;
        let rank: Option<u16> = item.getattr("rank").ok().and_then(|r| r.extract().ok());
        let props_obj = item.getattr("props")?;
        let props_dict = props_obj.downcast::<PyDict>()?;
        (src, dst, label, rank, py_props_to_hashmap(props_dict)?)
    };

    Ok(BulkEdge { src, dst, label, props: props_map, rank })
}

#[pyclass(unsendable)]
struct PyGraph {
    graph: Option<Graph>,
}

fn parse_schema_mode(mode: &str) -> PyResult<SchemaMode> {
    match mode.to_ascii_lowercase().as_str() {
        "strict" => Ok(SchemaMode::Strict),
        "auto" => Ok(SchemaMode::Auto),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid SchemaMode: '{other}'. Expected 'strict' or 'auto'."
        ))),
    }
}

fn parse_edge_mode(mode: &str) -> PyResult<EdgeMode> {
    match mode.to_ascii_lowercase().as_str() {
        "single" => Ok(EdgeMode::Single),
        "multi" => Ok(EdgeMode::Multi),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid EdgeMode: '{other}'. Expected 'single' or 'multi'."
        ))),
    }
}

fn parse_entity_type(entity_type: &str) -> PyResult<VectorEntityType> {
    match entity_type.to_ascii_lowercase().as_str() {
        "vertex" => Ok(VectorEntityType::Vertex),
        "edge" => Ok(VectorEntityType::Edge),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid VectorEntityType: '{other}'. Expected 'vertex' or 'edge'."
        ))),
    }
}

fn parse_distance_metric(metric: &str) -> PyResult<DistanceMetric> {
    match metric.to_ascii_lowercase().as_str() {
        "cosine" => Ok(DistanceMetric::Cosine),
        "euclidean" | "l2" => Ok(DistanceMetric::Euclidean),
        "dot_product" | "dot" => Ok(DistanceMetric::DotProduct),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid DistanceMetric: '{other}'. Expected 'cosine', 'euclidean' (or 'l2'), or 'dot_product'."
        ))),
    }
}

fn parse_ann_algorithm(algorithm: &str, m: usize, ef_construction: usize, ef_search: usize) -> PyResult<AnnAlgorithm> {
    match algorithm.to_ascii_lowercase().as_str() {
        "hnsw" => Ok(AnnAlgorithm::Hnsw(
            HnswConfig::default().with_m(m).with_ef_construction(ef_construction).with_ef_search(ef_search),
        )),
        "brute_force" | "exact" => Ok(AnnAlgorithm::BruteForce),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid AnnAlgorithm: '{other}'. Expected 'hnsw' or 'brute_force'."
        ))),
    }
}

fn parse_quantization(quantization: &str) -> PyResult<Quantization> {
    match quantization.to_ascii_lowercase().as_str() {
        "f16" => Ok(Quantization::F16),
        "f32" => Ok(Quantization::F32),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid Quantization: '{other}'. Expected 'f16' or 'f32'."
        ))),
    }
}

#[pymethods]
impl PyGraph {
    #[staticmethod]
    fn open(path: &str) -> PyResult<Self> {
        let path = PathBuf::from(path);
        let graph = Graph::open(path).map_err(store_error_to_pyerr)?;
        Ok(Self { graph: Some(graph) })
    }

    #[staticmethod]
    #[pyo3(signature = (path, *, mode = "auto", edge_mode = "single", storage = None, index = None))]
    fn open_with_options(
        path: &str,
        mode: &str,
        edge_mode: &str,
        storage: Option<&Bound<'_, PyDict>>,
        index: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let path = PathBuf::from(path);

        let schema_mode = parse_schema_mode(mode)?;
        let em = parse_edge_mode(edge_mode)?;

        let mut rocks = RocksOptions::default();
        if let Some(d) = storage {
            if let Some(v) = d.get_item("block_cache_size")? {
                rocks.block_cache_size = v.extract()?;
            }
            if let Some(v) = d.get_item("write_buffer_size")? {
                rocks.write_buffer_size = v.extract()?;
            }
            if let Some(v) = d.get_item("max_write_buffer_number")? {
                rocks.max_write_buffer_number = v.extract()?;
            }
            if let Some(v) = d.get_item("max_background_jobs")? {
                rocks.max_background_jobs = v.extract()?;
            }
            if let Some(v) = d.get_item("vertex_block_size")? {
                rocks.vertex_block_size = v.extract()?;
            }
            if let Some(v) = d.get_item("edge_block_size")? {
                rocks.edge_block_size = v.extract()?;
            }
            if let Some(v) = d.get_item("cache_index_and_filter_blocks")? {
                rocks.cache_index_and_filter_blocks = v.extract()?;
            }
        }

        let mut idx = IndexOptions::default();
        if let Some(d) = index {
            if let Some(v) = d.get_item("default_memory_limit")? {
                idx.default_limit = Some(VectorIndexLimit::new(v.extract()?));
            }
            if let Some(overrides) = d.get_item("per_index")? {
                let list = overrides.downcast::<PyList>()?;
                let mut vec = Vec::with_capacity(list.len());
                for o in list.iter() {
                    let o = o.downcast::<PyDict>()?;
                    let et_val = o.get_item("entity_type")?;
                    let et = if let Some(ref item) = et_val {
                        if let Ok(byte_val) = item.extract::<u8>() {
                            match byte_val {
                                1 => VectorEntityType::Edge,
                                _ => VectorEntityType::Vertex,
                            }
                        } else if let Ok(str_val) = item.extract::<String>() {
                            parse_entity_type(&str_val)?
                        } else {
                            VectorEntityType::Vertex
                        }
                    } else {
                        VectorEntityType::Vertex
                    };
                    let prop: String = o.get_item("property")?.map(|v| v.extract()).transpose()?.unwrap_or_default();
                    let mut entry = PerIndexOptions::new(et, prop);
                    if let Some(v) = o.get_item("memory_limit_bytes")? {
                        entry = entry.with_memory_limit(VectorIndexLimit::new(v.extract()?));
                    }
                    vec.push(entry);
                }
                idx.per_index = vec;
            }
        }

        let options =
            GraphOptions::default().with_mode(schema_mode).with_edge_mode(em).with_storage(rocks).with_index(idx);
        let graph = Graph::open_with_options(path, options).map_err(store_error_to_pyerr)?;
        Ok(Self { graph: Some(graph) })
    }

    fn read(&self) -> PyResult<PyReadSession> {
        let g =
            self.graph.as_ref().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Graph is already closed"))?;
        Ok(PyReadSession { session: g.read() })
    }

    fn begin(&self) -> PyResult<PyTxnSession> {
        let g =
            self.graph.as_ref().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Graph is already closed"))?;
        Ok(PyTxnSession { session: Some(g.begin()) })
    }

    fn open_schema(&self) -> PyResult<PySchemaSession> {
        let g =
            self.graph.as_ref().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Graph is already closed"))?;
        Ok(PySchemaSession { session: Some(g.open_schema()) })
    }

    fn open_bulk_loader(slf: Py<Self>, py: Python<'_>) -> PyResult<PyBulkLoader> {
        let borrow = slf.borrow(py);
        let g = borrow
            .graph
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Graph is already closed"))?;
        let loader = g.open_bulk_loader().map_err(store_error_to_pyerr)?;
        // Safety: PyBulkLoader keeps `slf` (Py<PyGraph>) alive, guaranteeing that `Graph` outlives `BulkLoader`.
        let loader_static: BulkLoader<'static> = unsafe { std::mem::transmute(loader) };
        drop(borrow);
        Ok(PyBulkLoader { _graph: slf, loader: Some(loader_static) })
    }

    fn index_manager(&self) -> PyResult<PyIndexManager> {
        let g =
            self.graph.as_ref().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Graph is already closed"))?;
        Ok(PyIndexManager { manager: g.index_manager() })
    }

    fn close(&mut self) -> PyResult<()> {
        if let Some(g) = self.graph.take() {
            g.close().map_err(store_error_to_pyerr)
        } else {
            Ok(())
        }
    }
}

#[pyclass(unsendable)]
struct PySchemaSession {
    session: Option<SchemaSession>,
}

#[pymethods]
impl PySchemaSession {
    fn add_vertex_label(&mut self, name: &str) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        s.add_vertex_label(name);
        Ok(())
    }

    fn add_edge_label(&mut self, name: &str) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        s.add_edge_label(name);
        Ok(())
    }

    fn add_property_key(&mut self, name: &str, data_type: u8) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        let dt = DataType::from_u8(data_type)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(format!("Invalid DataType code {data_type}")))?;
        s.add_property_key(name, dt);
        Ok(())
    }

    fn set_edge_mode(&mut self, mode: &str) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        let em = parse_edge_mode(mode)?;
        s.set_edge_mode(em);
        Ok(())
    }

    fn set_schema_mode(&mut self, mode: &str) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        let sm = parse_schema_mode(mode)?;
        s.set_schema_mode(sm);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (entity_type, property, dimension, *, metric = "cosine", algorithm = "hnsw", m = 16, ef_construction = 200, ef_search = 50, quantization = "f16"))]
    fn add_vector_index(
        &mut self,
        entity_type: &str,
        property: &str,
        dimension: usize,
        metric: &str,
        algorithm: &str,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
        quantization: &str,
    ) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        let et = parse_entity_type(entity_type)?;
        let met = parse_distance_metric(metric)?;
        let alg = parse_ann_algorithm(algorithm, m, ef_construction, ef_search)?;
        let quant = parse_quantization(quantization)?;
        let config = VectorIndexConfig::new(property, et, dimension, met, alg).with_quantization(quant);
        s.add_vector_index(config);
        Ok(())
    }

    fn drop_vector_index(&mut self, entity_type: &str, property: &str) -> PyResult<()> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        let et = parse_entity_type(entity_type)?;
        s.drop_vector_index(et, property);
        Ok(())
    }

    fn commit(&mut self) -> PyResult<()> {
        let s = self
            .session
            .take()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("SchemaSession already closed"))?;
        s.commit().map_err(store_error_to_pyerr)?;
        Ok(())
    }
}

#[pyclass(unsendable)]
struct PyBulkLoader {
    _graph: Py<PyGraph>,
    loader: Option<BulkLoader<'static>>,
}

#[pymethods]
impl PyBulkLoader {
    fn with_work_dir(&mut self, path: &str) -> PyResult<()> {
        let loader =
            self.loader.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        self.loader = Some(loader.with_work_dir(path));
        Ok(())
    }

    fn with_max_sst_size(&mut self, bytes: usize) -> PyResult<()> {
        let loader =
            self.loader.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        self.loader = Some(loader.with_max_sst_size(bytes));
        Ok(())
    }

    fn with_max_memory(&mut self, bytes: usize) -> PyResult<()> {
        let loader =
            self.loader.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        self.loader = Some(loader.with_max_memory(bytes));
        Ok(())
    }

    fn load_vertices(&mut self, vertices: &Bound<'_, PyAny>) -> PyResult<()> {
        let loader = self
            .loader
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        let mut rust_vertices = Vec::new();
        for item in vertices.iter()? {
            let item = item?;
            let bv = py_to_bulk_vertex(&item)?;
            rust_vertices.push(bv);
        }
        loader.load_vertices(rust_vertices).map_err(store_error_to_pyerr)?;
        Ok(())
    }

    fn load_edges(&mut self, edges: &Bound<'_, PyAny>) -> PyResult<()> {
        let loader = self
            .loader
            .as_mut()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        let mut rust_edges = Vec::new();
        for item in edges.iter()? {
            let item = item?;
            let be = py_to_bulk_edge(&item)?;
            rust_edges.push(be);
        }
        loader.load_edges(rust_edges).map_err(store_error_to_pyerr)?;
        Ok(())
    }

    fn commit(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        let loader =
            self.loader.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("BulkLoader already closed"))?;
        let stats = loader.commit().map_err(store_error_to_pyerr)?;
        let dict = PyDict::new_bound(py);
        dict.set_item("vertices_written", stats.vertices_written)?;
        dict.set_item("edges_written", stats.edges_written)?;
        dict.set_item("sst_files", stats.sst_files)?;
        dict.set_item("duration_secs", stats.duration_secs)?;
        Ok(dict.into())
    }
}

#[pyclass(unsendable)]
struct PyIndexManager {
    manager: IndexManager,
}

#[pymethods]
impl PyIndexManager {
    fn rebuild(&self, entity_type: &str, property: &str) -> PyResult<()> {
        let et = parse_entity_type(entity_type)?;
        self.manager.rebuild(et, property).map_err(store_error_to_pyerr)
    }

    fn save(&self, entity_type: &str, property: &str) -> PyResult<()> {
        let et = parse_entity_type(entity_type)?;
        self.manager.save(et, property).map_err(store_error_to_pyerr)
    }

    fn save_all(&self) -> PyResult<()> {
        self.manager.save_all().map_err(store_error_to_pyerr)
    }
}

#[pyclass(unsendable)]
struct PyReadSession {
    session: ReadSession,
}

#[pymethods]
impl PyReadSession {
    fn _execute(&mut self, py: Python<'_>, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<PyObject> {
        let results = self.session.execute(bytes, prop_keys).map_err(store_error_to_pyerr)?;
        values_to_py_list(py, results)
    }

    fn _explain(&mut self, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<String> {
        self.session.explain(bytes, prop_keys).map_err(store_error_to_pyerr)
    }
}

#[pyclass(unsendable)]
struct PyTxnSession {
    session: Option<TxnSession>,
}

#[pymethods]
impl PyTxnSession {
    fn _execute(&mut self, py: Python<'_>, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<PyObject> {
        let session =
            self.session.as_mut().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        let results = session.execute(bytes, prop_keys).map_err(store_error_to_pyerr)?;
        values_to_py_list(py, results)
    }

    fn _explain(&mut self, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<String> {
        let session =
            self.session.as_mut().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        session.explain(bytes, prop_keys).map_err(store_error_to_pyerr)
    }

    fn commit(mut slf: PyRefMut<'_, Self>) -> PyResult<()> {
        let session =
            slf.session.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        session.commit().map_err(store_error_to_pyerr)?;
        Ok(())
    }

    fn rollback(mut slf: PyRefMut<'_, Self>) -> PyResult<()> {
        let session =
            slf.session.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        session.rollback();
        Ok(())
    }
}

#[pymodule]
fn _rocksgraph(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraph>()?;
    m.add_class::<PyIndexManager>()?;
    m.add_class::<PyReadSession>()?;
    m.add_class::<PyTxnSession>()?;
    m.add_class::<PySchemaSession>()?;
    m.add_class::<PyBulkLoader>()?;
    errors::register(m)?;
    Ok(())
}
