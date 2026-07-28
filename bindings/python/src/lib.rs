use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use rocksgraph::{Graph, ReadSession, TxSession, Value};
use std::path::PathBuf;

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
            for (k, vals) in v.properties {
                if let Some(val) = vals.into_iter().next() {
                    props.set_item(k.to_string(), value_to_py(py, val)?)?;
                }
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

#[pyclass(unsendable)]
struct PyGraph {
    graph: Graph,
}

#[pymethods]
impl PyGraph {
    #[staticmethod]
    fn open(path: &str) -> PyResult<Self> {
        let path = PathBuf::from(path);
        let graph = Graph::open(path).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        Ok(Self { graph })
    }

    fn read(&self) -> PyReadSession {
        PyReadSession { session: self.graph.read() }
    }

    fn tx(&self) -> PyTxSession {
        PyTxSession { session: Some(self.graph.begin()) }
    }
}

#[pyclass(unsendable)]
struct PyReadSession {
    session: ReadSession,
}

#[pymethods]
impl PyReadSession {
    fn _execute(&mut self, py: Python<'_>, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<PyObject> {
        let results = self
            .session
            .execute(bytes, prop_keys)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        values_to_py_list(py, results)
    }
}

#[pyclass(unsendable)]
struct PyTxSession {
    session: Option<TxSession>,
}

#[pymethods]
impl PyTxSession {
    fn _execute(&mut self, py: Python<'_>, bytes: &[u8], prop_keys: Option<Vec<String>>) -> PyResult<PyObject> {
        let session =
            self.session.as_mut().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        let results = session
            .execute(bytes, prop_keys)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        values_to_py_list(py, results)
    }

    fn commit(mut slf: PyRefMut<'_, Self>) -> PyResult<()> {
        let session =
            slf.session.take().ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Session already closed"))?;
        session.commit().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
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
    m.add_class::<PyReadSession>()?;
    m.add_class::<PyTxSession>()?;
    Ok(())
}
