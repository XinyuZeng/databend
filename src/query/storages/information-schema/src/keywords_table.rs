// Copyright 2022 Datafuse Labs.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::BTreeMap;
use std::sync::Arc;

use common_catalog::table::Table;
use common_meta_app::schema::TableIdent;
use common_meta_app::schema::TableInfo;
use common_meta_app::schema::TableMeta;
use common_storages_view::view_table::ViewTable;
use common_storages_view::view_table::QUERY;
pub struct KeywordsTable {}

impl KeywordsTable {
    pub fn create(table_id: u64) -> Arc<dyn Table> {
        // TODO(veeupup): add more keywords in keywords table
        let query = "SELECT 'CREATE' AS WORD, 1 AS RESERVED";

        let mut options = BTreeMap::new();
        options.insert(QUERY.to_string(), query.to_string());
        let table_info = TableInfo {
            desc: "'information_schema'.'keywords'".to_string(),
            name: "keywords".to_string(),
            ident: TableIdent::new(table_id, 0),
            meta: TableMeta {
                options,
                engine: "VIEW".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        ViewTable::create(table_info)
    }
}