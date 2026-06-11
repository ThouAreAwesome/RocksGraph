// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

use smol_str::SmolStr;

/// Name of a property key.
///
/// Stack-allocated for strings up to 22 bytes; heap-allocated only for
/// unusually long key names.  No interning or numeric mapping — the raw
/// string is the identity.
pub type PropKey = SmolStr;

pub const ID: PropKey = SmolStr::new_static("id");
pub const LABEL: PropKey = SmolStr::new_static("label");
