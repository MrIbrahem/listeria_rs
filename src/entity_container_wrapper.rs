use crate::listeria_list::ListeriaList;
use crate::result_cell_part::PartWithReference;
use crate::result_cell_part::ResultCellPart;
use crate::result_row::ResultRow;
use crate::{LinksType, SparqlValue};
use tempfile::NamedTempFile;
use std::collections::HashMap;
use std::sync::Arc;
//use std::sync::Mutex;
use wikibase::entity::*;
use wikibase::entity_container::EntityContainer;
use wikibase::mediawiki::api::Api;
use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use wikibase::snak::SnakDataType;
//use std::io::{SeekFrom, Write};
//use std::io::prelude::*;

const MAX_LOCAL_CACHED_ENTITIES: usize = usize::MAX; //100;

#[derive(Clone)]
pub struct EntityContainerWrapper {
    entities: EntityContainer,
    pickledb: Option<Arc<PickleDb>>,
    pickledb_filename: Option<Arc<NamedTempFile>>,
    // file_handle: Option<Arc<Mutex<std::fs::File>>>,
    // entity2pos: HashMap<String,(u64,u64)>,
}

impl std::fmt::Debug for EntityContainerWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntityContainerWrapper")
         .field("entities", &self.entities)
         .field("pickledb_filename", &self.pickledb_filename)
         .finish()
    }
}

impl EntityContainerWrapper {
    pub fn new() -> Self {
        Self {
            entities: EntityContainer::new(),
            pickledb: None,
            pickledb_filename: None,
            // file_handle: None,
            // entity2pos: HashMap::new(),
        }
    }
/*
    fn hashfile_add_entity(&mut self, entity_id: &str, json: serde_json::Value) {
        match &self.file_handle {
            Some(fh) => {
                let mut fh = fh.lock().unwrap();
                let start = fh.metadata().unwrap().len();
                let json_string = json.to_string();
                println!("Writing data for {entity_id}");
                write!(fh, "{json_string}").unwrap();
                fh.sync_all().unwrap();
                let end = fh.metadata().unwrap().len();
                let len = end-start;
                self.entity2pos.insert(entity_id.to_string(), (start,len));        
            },
            None => panic!("add_entity: No file handle"),
        }
    }

    fn hashfile_create(&mut self) {
        self.file_handle = Some(Arc::new(Mutex::new(tempfile().unwrap())));
    }

    fn hashfile_get_entity(&self, entity_id: &str) -> Option<Entity> {
        let (start,len) = self.entity2pos.get(entity_id)?;
        match &self.file_handle {
            Some(fh) => {
                let mut fh = fh.lock().ok()?;
                fh.seek(SeekFrom::Start(*start)).ok()?;
                let mut buffer = Vec::with_capacity(*len as usize);
                fh.read_exact(&mut buffer).ok()?;
                let json_string = String::from_utf8(buffer).ok()?;
                let json = serde_json::from_str(&json_string).ok()?;
                Entity::new_from_json(&json).ok()
        
            },
            None => panic!("add_entity: No file handle"),
        }
    }
*/
    pub async fn load_entities(&mut self, api: &Api, ids: &Vec<String>) -> Result<(), String> {
        self.load_entities_max_size(api, ids, MAX_LOCAL_CACHED_ENTITIES).await
    }

    pub async fn load_entities_max_size(&mut self, api: &Api, ids: &Vec<String>, max_entities: usize) -> Result<(), String> {
        let ids = self.entities.unique_shuffle_entity_ids(ids).unwrap();
        if ids.len()>max_entities { // Use pickledb disk cache
            // self.hashfile_create();
            self.pickledb_filename = Some(Arc::new(            
                match  NamedTempFile::new() {
                    Ok(filename) => filename,
                    Err(e) => return Err(format!("Error loading entities: {}", &e.to_string()))
                }
            ));
            let temp_filename = self.pickledb_filename.as_ref().unwrap().path().to_str().unwrap();
            let mut db = PickleDb::new(
                temp_filename,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            );
            let chunks = ids.chunks(max_entities) ;
            for chunk in chunks {
                if let Err(e) = self.entities.load_entities(api, &chunk.into()).await {
                    return Err(format!("Error loading entities: {:?}", &e))
                }
                for entity_id in chunk {
                    if let Some(entity) = self.entities.get_entity(entity_id) {
                        let json = entity.to_json();
                        //let _ = self.hashfile_add_entity(&entity.id(), json);
                        db.set(&entity.id(), &json).unwrap();
                    }
                }
                self.entities.clear();
            }
            self.pickledb = Some(Arc::new(db));
            Ok(())
        } else {
            match self.entities.load_entities(api, &ids).await {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("Error loading entities: {:?}", &e)),
            }
        }
    }

    pub fn get_entity(&self, entity_id: &str) -> Option<Entity> {
        if let Some(entity) = self.entities.get_entity(entity_id) {
            return Some(entity)
        }
        // self.hashfile_get_entity(entity_id)
        let json = self.pickledb.as_ref()?.get::<serde_json::Value>(entity_id)?;
        Entity::new_from_json(&json).ok()
    }

    pub fn get_local_entity_label(&self, entity_id: &str, language: &str) -> Option<String> {
        self.get_entity(entity_id)?
            .label_in_locale(language)
            .map(|s| s.to_string())
    }

    pub fn entity_to_local_link(
        &self,
        item: &str,
        wiki: &str,
        language: &str,
    ) -> Option<ResultCellPart> {
        let entity = match self.get_entity(item) {
            Some(e) => e,
            None => return None,
        };
        let page = match entity.sitelinks() {
            Some(sl) => sl
                .iter()
                .filter(|s| *s.site() == wiki)
                .map(|s| s.title().to_string())
                .next(),
            None => None,
        }?;
        //let title = wikibase::mediawiki::title::Title::new_from_full(page,&mw_api);
        let label = self
            .get_local_entity_label(item, language)
            .unwrap_or_else(|| page.clone());
        Some(ResultCellPart::LocalLink((page, label, false)))
    }

    pub async fn get_result_row(
        &self,
        entity_id: &str,
        sparql_rows: &[&HashMap<String, SparqlValue>],
        list: &ListeriaList,
    ) -> Option<ResultRow> {
        if sparql_rows.is_empty() {
            return None;
        }
        if let LinksType::Local = list.template_params().links {
            let entity = match self.get_entity(entity_id) {
                Some(e) => e,
                None => return None,
            };
            let page = match entity.sitelinks() {
                Some(sl) => sl
                    .iter()
                    .filter(|s| *s.site() == *list.wiki())
                    .map(|s| s.title().to_string())
                    .next(),
                None => None,
            };
            page.as_ref()?; // return None if no page on this wiki
        }

        let mut row = ResultRow::new(entity_id);
        row.from_columns(list, sparql_rows).await;
        Some(row)
    }

    pub fn external_id_url(&self, prop: &str, id: &str) -> Option<String> {
        let pi = self.get_entity(prop)?;
        pi.claims_with_property("P1630")
            .iter()
            .filter_map(|s| {
                let data_value = s.main_snak().data_value().to_owned()?;
                match data_value.value() {
                    wikibase::Value::StringValue(s) => {
                        Some(s.to_owned().replace("$1", &urlencoding::decode(&id).ok()?))
                    }
                    _ => None,
                }
            })
            .next()
    }

    pub fn get_datatype_for_property(&self, prop: &str) -> SnakDataType {
        match self.get_entity(prop) {
            Some(entity) => match entity {
                Entity::Property(p) => match p.datatype() {
                    Some(t) => t.to_owned(),
                    None => SnakDataType::String,
                },
                _ => SnakDataType::String,
            },
            None => SnakDataType::String,
        }
    }

    pub fn gather_entities_and_external_properties(
        &self,
        parts: &[PartWithReference],
    ) -> Vec<String> {
        let mut entities_to_load = vec![];
        for part_with_reference in parts {
            match &part_with_reference.part {
                ResultCellPart::Entity((item, true)) => {
                    entities_to_load.push(item.to_owned());
                }
                ResultCellPart::ExternalId((property, _id)) => {
                    entities_to_load.push(property.to_owned());
                }
                ResultCellPart::SnakList(v) => self
                    .gather_entities_and_external_properties(&v)
                    .iter()
                    .for_each(|entity_id| entities_to_load.push(entity_id.to_string())),
                _ => {}
            }
        }
        entities_to_load
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pickledb() {
        let mut ecw = EntityContainerWrapper::new();
        let api = wikibase::mediawiki::api::Api::new("https://www.wikidata.org/w/api.php").await.unwrap();
        let ids = ["Q1","Q2","Q3","Q4","Q5"].iter().map(|s|s.to_string()).collect();
        ecw.load_entities_max_size(&api, &ids, 2).await.unwrap();
        assert_eq!(ecw.entities.len(),0);

        let path = ecw.pickledb_filename.as_ref().unwrap().path();
        let len = std::fs::metadata(path).unwrap().len();
        assert!(len>0);

        let e2 = ecw.get_entity("Q2").unwrap();
        assert_eq!(e2.id(),"Q2");
    }
}