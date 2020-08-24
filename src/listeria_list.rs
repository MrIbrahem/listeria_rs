use std::collections::HashSet;
use crate::*;
use wikibase::entity::*;
use wikibase::snak::SnakDataType;
use wikibase::entity_container::EntityContainer;
use result_cell::*;

#[derive(Debug, Clone)]
pub struct ListeriaList {
    page_params: PageParams,
    template: Template,
    columns: Vec<Column>,
    params: TemplateParams,
    sparql_rows: Vec<HashMap<String, SparqlValue>>,
    sparql_first_variable: Option<String>,
    entities: EntityContainer,
    results:Vec<ResultRow>,
    shadow_files: Vec<String>,
    local_page_cache: HashMap<String,bool>,
    section_id_to_name: HashMap<usize,String>,
    wb_api: Arc<Api>, // TODO Arc (and all the other wb_api as well)
}

impl ListeriaList {
    pub fn new(template:Template,page_params:PageParams) -> Self {
        let wb_api = page_params.wb_api.clone() ;
        Self {
            page_params,
            template,
            columns: vec![],
            params:TemplateParams::new(),
            sparql_rows: vec![],
            sparql_first_variable: None,
            entities: EntityContainer::new(),
            results: vec![],
            shadow_files: vec![],
            local_page_cache: HashMap::new(),
            section_id_to_name: HashMap::new(),
            wb_api,
        }
    }

    pub async fn process(&mut self) -> Result<(),String> {
        self.process_template().await?;
        self.run_query().await?;
        self.load_entities().await?;
        self.generate_results().await?;
        self.process_results().await?;
        Ok(())
    }

    pub fn results(&self) -> &Vec<ResultRow> {
        &self.results
    }

    pub fn columns(&self) -> &Vec<Column> {
        &self.columns
    }

    pub fn shadow_files(&self) -> &Vec<String> {
        &self.shadow_files
    }

    pub fn sparql_rows(&self) -> &Vec<HashMap<String, SparqlValue>> {
        &self.sparql_rows
    }

    pub fn local_file_namespace_prefix(&self) -> &String {
        self.page_params.local_file_namespace_prefix()
    }

    pub fn section_name(&self,id:usize) -> Option<&String> {
        self.section_id_to_name.get(&id)
    }

    pub async fn process_template(&mut self) -> Result<(), String> {
        let template = self.template.clone();
        match template.params.get("columns") {
            Some(columns) => {
                columns.split(',').for_each(|part| {
                    let s = part.to_string();
                    self.columns.push(Column::new(&s));
                });
            }
            None => self.columns.push(Column::new(&"item".to_string())),
        }

        self.params = TemplateParams::new_from_params(&template) ;
        if let Some(l) = template.params.get("language") { self.page_params.language = l.to_lowercase() }
        if let Some(s) = template.params.get("links") { self.params.links = LinksType::new_from_string(s.to_string()) }

        let wikibase = &self.params.wikibase ;
        println!("WIKIBASE: {}",&wikibase);
        self.wb_api = match self.page_params.config.get_wbapi(&wikibase.to_lowercase()) {
            Some(api) => api.clone(),
            None => return Err(format!("No wikibase setup configured for '{}'",&wikibase)),
        } ;

        Ok(())
    }

    pub fn language(&self) -> &String {
        &self.page_params.language
    }


    async fn cache_local_page_exists(&mut self,page:String) {
        let params: HashMap<String, String> = vec![
            ("action", "query"),
            ("prop", ""),
            ("titles", page.as_str()),
        ]
        .iter()
        .map(|x| (x.0.to_string(), x.1.to_string()))
        .collect();

        let result = match self
            .page_params
            .mw_api
            .lock()
            .await
            .get_query_api_json(&params)
            .await {
                Ok(r) => r,
                Err(_e) => return
            };
            
        let page_exists = match result["query"]["pages"].as_object() {
            Some(obj) => {
                obj
                .iter()
                .filter(|(_k,v)|v["missing"].as_str().is_some())
                .count()==0 // No "missing"=existing
            }
            None => false // Dunno
        };
        self.local_page_cache.insert(page,page_exists);
    }

    pub fn local_page_exists(&self,page:&str) -> bool {
        *self.local_page_cache.get(&page.to_string()).unwrap_or(&false)
    }

    pub fn normalize_page_title(&self,s: &str) -> String {
        // TODO use page to find out about first character capitalization on the current wiki
        if s.len() < 2 {
            return s.to_string();
        }
        let (first_letter, the_rest) = s.split_at(1);
        first_letter.to_uppercase() + the_rest
    }

    pub fn get_location_template(&self, lat: f64, lon: f64, entity_id: Option<String>, region: Option<String> ) -> String {
        self
            .page_params
            .config
            .get_location_template(&self.page_params.wiki)
            .replace("$LAT$",&format!("{}",lat))
            .replace("$LON$",&format!("{}",lon))
            .replace("$ITEM$",&entity_id.unwrap_or(String::new()))
            .replace("$REGION$",&region.unwrap_or(String::new()))
    }

    pub fn thumbnail_size(&self) -> u64 {
        let default = self.page_params.config.default_thumbnail_size();
        match self.template.params.get("thumb") {
            Some(s) => s.parse::<u64>().ok().or(Some(default)).unwrap(),
            None => default,
        }
    }

    pub async fn run_sparql_query(&self, sparql: &str) -> Result<Value, String> {
        let endpoint = match self.wb_api.get_site_info_string("general", "wikibase-sparql") {
            Ok(endpoint) => { // SPARQL service given by site
                endpoint
            }
            _ => { // Override SPARQL service (hardcoded for Commons)
                "https://wcqs-beta.wmflabs.org/sparql"
            }
        } ;
        //println!("USING ENDPOINT {}",&endpoint);
        match self.wb_api.sparql_query_endpoint(sparql,endpoint).await {
            Ok(j) => Ok(j),
            Err(e) => return Err(format!("{:?}", &e)),
        }
    }

    pub async fn run_query(&mut self) -> Result<(), String> {
        let sparql = match self.template.params.get("sparql") {
            Some(s) => s,
            None => return Err(format!("No `sparql` parameter in {:?}", &self.template)),
        };

        // Return simulated results
        if self.page_params.simulate {
            match &self.page_params.simulated_sparql_results {
                Some(json_text) => {
                    let j = serde_json::from_str(&json_text).map_err(|e|e.to_string())?;
                    return self.parse_sparql(j);
                }
                None => {}
            }
        }

        let j = self.run_sparql_query(&sparql).await? ;

        if self.page_params.simulate {
            println!("{}\n{}\n",&sparql,&j);
        }
        self.parse_sparql(j)
    }

    fn parse_sparql(&mut self, j: Value) -> Result<(), String> {
        self.sparql_rows.clear();
        self.sparql_first_variable = None;
        
        // TODO force first_var to be "item" for backwards compatability?
        // Or check if it is, and fail if not?
        let first_var = match j["head"]["vars"].as_array() {
            Some(a) => match a.get(0) {
                Some(v) => v.as_str().ok_or("Can't parse first variable")?.to_string(),
                None => return Err("Bad SPARQL head.vars".to_string()),
            },
            None => return Err("Bad SPARQL head.vars".to_string()),
        };
        self.sparql_first_variable = Some(first_var);

        let bindings = j["results"]["bindings"]
            .as_array()
            .ok_or("Broken SPARQL results.bindings")?;
        for b in bindings.iter() {
            let mut row: HashMap<String, SparqlValue> = HashMap::new();
            for (k, v) in b.as_object().unwrap().iter() {
                match SparqlValue::new_from_json(&v) {
                    Some(v2) => row.insert(k.to_owned(), v2),
                    None => return Err(format!("Can't parse SPARQL value: {} => {:?}", &k, &v)),
                };
            }
            if row.is_empty() {
                continue;
            }
            self.sparql_rows.push(row);
        }
        Ok(())
    }

    pub async fn load_entities(&mut self) -> Result<(), String> {
        // Any columns that require entities to be loaded?
        // TODO also force if self.links is redlinks etc.
        if self
            .columns
            .iter()
            .filter(|c| match c.obj {
                ColumnType::Number => false,
                ColumnType::Item => false,
                ColumnType::Field(_) => false,
                _ => true,
            })
            .count()
            == 0
        {
            return Ok(());
        }

        let ids = self.get_ids_from_sparql_rows()?;
        if ids.is_empty() {
            return Err("No items to show".to_string());
        }
        match self.entities.load_entities(&self.wb_api, &ids).await {
            Ok(_) => {}
            Err(e) => return Err(format!("Error loading entities: {:?}", &e)),
        }

        self.label_columns();

        Ok(())
    }

    fn label_columns(&mut self) {
        self.columns = self
            .columns
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.generate_label(self);
                c
            })
            .collect();
    }

    fn get_ids_from_sparql_rows(&self) -> Result<Vec<String>, String> {
        let varname = self.get_var_name()?;

        // Rows
        let ids_tmp: Vec<String> = self
            .sparql_rows
            .iter()
            .filter_map(|row| match row.get(varname) {
                Some(SparqlValue::Entity(id)) => Some(id.to_string()),
                _ => None,
            })
            .collect();

        let mut ids: Vec<String> = vec![] ;
        ids_tmp.iter().for_each(|id|{
            if !ids.contains(id) {
                ids.push(id.to_string());
            }
        });

        // Can't sort/dedup, need to preserve original order

        // Column headers
        self.columns.iter().for_each(|c| match &c.obj {
            ColumnType::Property(prop) => {
                ids.push(prop.to_owned());
            }
            ColumnType::PropertyQualifier((prop, qual)) => {
                ids.push(prop.to_owned());
                ids.push(qual.to_owned());
            }
            ColumnType::PropertyQualifierValue((prop1, qual, prop2)) => {
                ids.push(prop1.to_owned());
                ids.push(qual.to_owned());
                ids.push(prop2.to_owned());
            }
            _ => {}
        });

        Ok(ids)
    }

    fn get_var_name(&self) -> Result<&String, String> {
        match &self.sparql_first_variable {
            Some(v) => Ok(v),
            None => Err("load_entities: sparql_first_variable is None".to_string()),
        }
    }

    pub fn get_local_entity_label(&self, entity_id: &str) -> Option<String> {
        self.entities
            .get_entity(entity_id.to_owned())?
            .label_in_locale(&self.page_params.language)
            .map(|s| s.to_string())
    }

    fn entity_to_local_link(&self, item: &str) -> Option<ResultCellPart> {
        let entity = match self.entities.get_entity(item.to_owned()) {
            Some(e) => e,
            None => return None,
        };
        let page = match entity.sitelinks() {
            Some(sl) => sl
                .iter()
                .filter(|s| *s.site() == self.page_params.wiki)
                .map(|s| s.title().to_string())
                .next(),
            None => None,
        }?;
        let label = self.get_local_entity_label(item).unwrap_or_else(|| page.clone());
        Some(ResultCellPart::LocalLink((page, label)))
    }


    pub fn get_filtered_claims(&self,e:&wikibase::entity::Entity,property:&str) -> Vec<wikibase::statement::Statement> {
        let mut ret : Vec<wikibase::statement::Statement> = e
            .claims_with_property(property)
            .iter()
            .map(|x|(*x).clone())
            .collect();

        if self.page_params.config.prefer_preferred() {
            let has_preferred = ret.iter().any(|x|*x.rank()==wikibase::statement::StatementRank::Preferred);
            if has_preferred {
                ret.retain(|x|*x.rank()==wikibase::statement::StatementRank::Preferred);
            }
            ret
        } else {
            ret
        }
    }

    pub async fn get_autodesc_description(&self, e:&Entity) -> Result<String,String> {
        if self.params.autodesc != Some("FALLBACK".to_string()) {
            return Err("Not used".to_string());
        }
        let url = format!("https://tools.wmflabs.org/autodesc/?q={}&lang={}&mode=short&links=wiki&format=json",e.id(),self.page_params.language);
        let api = self.page_params.mw_api.lock().await;
        let body = api
            .query_raw(&url,&api.no_params(),"GET")
            .await
            .map_err(|e|e.to_string())?;
        let json : Value = serde_json::from_str(&body).map_err(|e|e.to_string())?;
        match json["result"].as_str() {
            Some(result) => Ok(result.to_string()),
            None => Err("Not a valid autodesc result".to_string())
        }
    }

    async fn get_result_row(
        &self,
        entity_id: &str,
        sparql_rows: &[&HashMap<String, SparqlValue>],
    ) -> Option<ResultRow> {
        if let LinksType::Local = self.params.links {
            if !self.entities.has_entity(entity_id.to_owned()) {
                return None;
            }
        }

        let mut row = ResultRow::new(entity_id);
        row.from_columns(self,sparql_rows).await;
        Some(row)
    }

    pub async fn generate_results(&mut self) -> Result<(), String> {
        let varname = self.get_var_name()?;
        let orpi = self.params.one_row_per_item ;
        let mut results : Vec<ResultRow> = vec![] ;
        match orpi {
            true => {
                for id in self.get_ids_from_sparql_rows()?.iter() {
                    let sparql_rows: Vec<&HashMap<String, SparqlValue>> = self
                        .sparql_rows
                        .iter()
                        .filter(|row| match row.get(varname) {
                            Some(SparqlValue::Entity(v)) => v == id,
                            _ => false,
                        })
                        .collect();
                    if !sparql_rows.is_empty() {
                        let tmp = self.get_result_row(id,&sparql_rows).await ;
                        if let Some(x) = tmp {results.push(x);}
                    }
                }
            }
            false => {
                for row in self.sparql_rows.iter() {
                    if let Some(SparqlValue::Entity(id)) = row.get(varname) {
                        if let Some(x) = self.get_result_row(id, &[&row]).await {results.push(x);}
                    }
                }
            }
        } ;
        self.results = results ;
        Ok(())
    }

    fn localize_item_links_in_parts(&self,parts:&[ResultCellPart]) -> Vec<ResultCellPart> {
        parts.iter()
        .map(|part| match part {
            ResultCellPart::Entity((item, true)) => {
                match self.entity_to_local_link(&item) {
                    Some(ll) => ll,
                    None => part.to_owned(),
                }
            }
            ResultCellPart::SnakList(v) => {
                ResultCellPart::SnakList(self.localize_item_links_in_parts(v))
            }
            _ => part.to_owned(),
        })
        .collect()
    }


    fn process_items_to_local_links(&mut self) -> Result<(), String> {
        // Try to change items to local link
        // TODO mutate in place; fn in ResultRow. This is pathetic.
        self.results = self
            .results
            .iter()
            .map(|row|{
                let mut new_row = row.clone();
                let new_cells = row
                .cells()
                .iter()
                .map(|cell|
                    ResultCell::new_from_parts ( self.localize_item_links_in_parts(cell.parts()) )
                )
                .collect();
                new_row.set_cells(new_cells);
                new_row
            })
            .collect();
        Ok(())
    }


    async fn process_remove_shadow_files(&mut self) -> Result<(), String> {
        if !self.page_params.config.check_for_shadow_images(&self.page_params.wiki) {
            return Ok(())
        }
        let mut files_to_check = vec![] ;
        for row in self.results.iter() {
            for cell in row.cells() {
                for part in cell.parts() {
                    if let ResultCellPart::File(file) = part {
                        files_to_check.push(file);
                    }
                }
            }
        }
        files_to_check.sort_unstable();
        files_to_check.dedup();

        self.shadow_files.clear();

        // TODO better async
        for filename in files_to_check {
            let prefixed_filename = format!("{}:{}",self.page_params.local_file_namespace_prefix(),&filename) ;
            let params: HashMap<String, String> =
                vec![("action", "query"), ("titles", prefixed_filename.as_str()),("prop","imageinfo")]
                    .iter()
                    .map(|x| (x.0.to_string(), x.1.to_string()))
                    .collect();

            let j = match self.page_params.mw_api.lock().await.get_query_api_json(&params).await {
                Ok(j) => j,
                Err(_e) => json!({})
            };

            let mut could_be_local = false ;
            match j["query"]["pages"].as_object() {
                Some(results) => {
                    results.iter().for_each(|(_k, o)|{
                        match o["imagerepository"].as_str() {
                            Some("shared") => {},
                            _ => { could_be_local = true ; }
                        }
                    })
                }
                None => { could_be_local = true ; }
            };

            if could_be_local {
                self.shadow_files.push(filename.to_string());
            }
        }

        self.shadow_files.sort();

        // Remove shadow files from data table
        // TODO this is less than ideal in terms of pretty code...
        let shadow_files = self.shadow_files.clone();
        self.results.iter_mut().for_each(|row|{
            row.remove_shadow_files(&shadow_files);
        });

        Ok(())
    }

    fn process_redlinks_only(&mut self) -> Result<(), String> {
        if *self.get_links_type() != LinksType::RedOnly {
            return Ok(())
        }

        // Remove all rows with existing local page  
        // TODO better iter things
        self.results = self.results
            .iter()
            .filter(|row|{
                let entity = self.entities.get_entity(row.entity_id().to_owned()).unwrap();
                match entity.sitelinks() {
                    Some(sl) => {
                        sl
                        .iter()
                        .filter(|s| *s.site() == self.page_params.wiki)
                        .count() == 0
                    }
                    None => true, // No sitelinks, keep
                }
            })
            .cloned()
            .collect();
        Ok(())
    }

    async fn process_redlinks(&mut self) -> Result<(), String> {
        if *self.get_links_type() != LinksType::RedOnly && *self.get_links_type() != LinksType::Red {
            return Ok(())
        }

        // Cache if local pages exist
        let mut ids = vec![] ;
        self.results.iter().for_each(|row|{
            row.cells().iter().for_each(|cell|{
                cell.parts()
                    .iter()
                    .for_each(|part|{
                    if let ResultCellPart::Entity((id, _try_localize)) = part {
                        ids.push(id);
                    }
                })
            });
        });

        ids.sort();
        ids.dedup();
        let mut labels = vec![] ;
        for id in ids {
            if let Some(e) = self.get_entity(id.to_owned()) {
                if let Some(l) = e.label_in_locale(self.language()) { labels.push(l.to_string()); }
            }
        }

        labels.sort();
        labels.dedup();
        for label in labels {
            self.cache_local_page_exists(label).await;
        }

        Ok(())
    }

    fn get_datatype_for_property(&self,prop:&str) -> SnakDataType {
        match self.get_entity(prop) {
            Some(entity) => {
                match entity {
                    Entity::Property(p) => {
                        match p.datatype() {
                            Some(t) => t.to_owned(),
                            None => SnakDataType::String
                        }
                    }
                    _ => SnakDataType::String
                }
            }
            None => SnakDataType::String
        }
    }

    fn process_sort_results(&mut self) -> Result<(), String> {
        let sortkeys : Vec<String> ;
        let mut datatype = SnakDataType::String ; // Default
        match &self.params.sort {
            SortMode::Label => {
                sortkeys = self.results
                    .iter()
                    .map(|row|row.get_sortkey_label(&self))
                    .collect();
            }
            SortMode::FamilyName => {
                sortkeys = self.results
                    .iter()
                    .map(|row|row.get_sortkey_family_name(&self))
                    .collect();
            }
            SortMode::Property(prop) => {
                datatype = self.get_datatype_for_property(prop);
                sortkeys = self.results
                    .iter()
                    .map(|row|row.get_sortkey_prop(&prop,&self,&datatype))
                    .collect();
            }
            _ => return Ok(())
        }

        // Apply sortkeys
        if self.results.len() != sortkeys.len() { // Paranoia
            return Err("process_sort_results: sortkeys length mismatch".to_string());
        }
        self.results
            .iter_mut()
            .enumerate()
            .for_each(|(rownum, row)|row.set_sortkey(sortkeys[rownum].to_owned())) ;

        self.results.sort_by(|a, b| a.compare_to(b,&datatype));
        if self.params.sort_order == SortOrder::Descending {
            self.results.reverse()
        }

        //self.results.iter().for_each(|row|println!("{}: {}",&row.entity_id,&row.sortkey));
        Ok(())
    }

    pub fn process_assign_sections(&mut self) -> Result<(), String> {
        // TODO all SectionType options
        let section_property = match &self.params.section {
            SectionType::Property(p) => p ,
            SectionType::SparqlVariable(_v) => return Err("SPARQL variable section type not supported yet".to_string()),
            SectionType::None => return Ok(()) // Nothing to do
        } ;
        let datatype = self.get_datatype_for_property(section_property);

        let section_names = self.results
            .iter()
            .map(|row|row.get_sortkey_prop(section_property,self,&datatype))
            .collect::<Vec<String>>();
        //println!("{:?}/{:?}/{:?}",&section_property,&datatype,&section_names);
        
        // Count names
        let mut section_count = HashMap::new();
        section_names
            .iter()
            .for_each(|name|{
                let counter = section_count.entry(name).or_insert(0);
                *counter += 1 ;
            });
        
        // Remove low counts
        section_count.retain(|&_name,&mut count|count>=self.params.min_section);

        // Sort by section name
        let mut valid_section_names : Vec<String> = section_count.iter().map(|(k,_v)|k.to_string()).collect();
        valid_section_names.sort();
        /*
        // Sort by count, largest first
        valid_section_names.sort_by(|a, b| {
            let va = section_count.get(a).unwrap() ;
            let vb = section_count.get(b).unwrap() ;
            vb.partial_cmp(va).unwrap()
        } );
        */
        let misc_id = valid_section_names.len();
        valid_section_names.push("Misc".to_string());

        // TODO skip if no/one section?

        // name to id
        let name2id : HashMap<String,usize> = valid_section_names
            .iter()
            .enumerate()
            .map(|(num,name)|(name.to_string(),num))
            .collect();
        
        self.section_id_to_name = name2id
            .iter()
            .map(|x|(x.1.to_owned(),x.0.to_owned()))
            .collect();
        
        // println!("{:?}",&self.section_id_to_name);

        self.results
            .iter_mut()
            .enumerate()
            .for_each(|(num,row)|{
                let section_name = match section_names.get(num) {
                    Some(name) => name,
                    None => return // Err(format!("process_assign_sections: No name for {}", num)),
                };
                let section_id = match name2id.get(section_name) {
                    Some(id) => *id,
                    None => misc_id,
                } ;
                row.set_section(section_id);
            });
        
        Ok(())
    }

    async fn get_region_for_entity_id(&self, entity_id: &String) -> Option<String> {
        let sparql = format!("SELECT ?q ?x {{ wd:{} wdt:P131* ?q . ?q wdt:P300 ?x }}", entity_id) ;
        let j = self.run_sparql_query(&sparql).await.ok()?;
        match j["results"]["bindings"].as_array() {
            Some(a) => {
                let mut region = String::new();
                a.iter().for_each(|b|{
                    match b["x"]["type"].as_str() {
                        Some("literal") => {}
                        _ => return
                    }
                    match b["x"]["value"].as_str() {
                        Some(r) => {
                            if r.len() > region.len() {
                                region = r.to_string();
                            }
                        }
                        None => {}
                    }
                });
                if region.is_empty() { None } else { Some(region) }
            }
            None => None
        }
        /*
        $ret = 'x' ;
        $sparql = "SELECT ?q ?x { wd:Q$q wdt:P131* ?q . ?q wdt:P300 ?x }" ;
        $j = getSPARQL ( $sparql ) ;
        if ( !isset($j->results) or !isset($j->results->bindings) or count($j->results->bindings) == 0 ) return $ret ;
        foreach ( $j->results->bindings AS $b ) {
            if ( !isset($b->x) ) continue ;
            if ( $b->x->type != 'literal' ) continue ;
            $region = $b->x->value ;
            if ( strlen ( $region ) > strlen ( $ret ) ) $ret = $region ;
        }
        return $ret ;
        */
    }

    fn do_get_regions(&self) -> bool {
        self.page_params.config.location_regions().contains(self.wiki())
    }

    pub async fn process_regions(&mut self) -> Result<(), String> {
        if !self.do_get_regions() {
            return Ok(());
        }

        let mut entity_ids = HashSet::new() ;
        self.results.iter().for_each(|row|{
            row.cells().iter().for_each(|cell|{
                cell.parts().iter().for_each(|part|{
                    match part {
                        ResultCellPart::Location((_lat,_lon,_region)) => {
                            entity_ids.insert(row.entity_id().to_string());
                            //*region = self.get_region_for_entity_id(row.entity_id()).await ;
                        }
                        _ => {}
                    }
                });
            });
        });

        let mut entity_id2region = HashMap::new();
        for entity_id in entity_ids {
            match self.get_region_for_entity_id(&entity_id).await {
                Some(region) => { entity_id2region.insert(entity_id,region); }
                None => {}
            }
        }

        for row in self.results.iter_mut() {
            let the_region = match entity_id2region.get(row.entity_id()) {
                Some(r) => r,
                None => continue,
            };
            for cell in row.cells_mut().iter_mut() {
                for part in cell.parts_mut().iter_mut() {
                    match part {
                        ResultCellPart::Location((_lat,_lon,region)) => {
                            *region = Some(the_region.clone()) ;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
    
    pub async fn process_results(&mut self) -> Result<(), String> {
        self.gather_and_load_items().await? ;
        self.process_redlinks_only()?;
        self.process_items_to_local_links()?;
        self.process_redlinks().await?;
        self.process_remove_shadow_files().await?;
        self.process_sort_results()?;
        self.process_assign_sections()?;
        self.process_regions().await?;
        Ok(())
    }

    pub fn get_links_type(&self) -> &LinksType {
        &self.params.links // TODO duplicate code
    }

    pub fn get_entity<S: Into<String>>(&self, entity_id: S) -> Option<wikibase::Entity> {
        self.entities.get_entity(entity_id)
    }

    pub fn external_id_url(&self, prop: &str, id: &str) -> Option<String> {
        let pi = self.entities.get_entity(prop.to_owned())?;
        pi.claims_with_property("P1630")
            .iter()
            .filter_map(|s| {
                let data_value = s.main_snak().data_value().to_owned()?;
                match data_value.value() {
                    wikibase::Value::StringValue(s) => 
                        Some(
                        s.to_owned()
                            .replace("$1", &urlencoding::decode(&id).ok()?),
                    ),
                    _ => None,
                }
            })
            .next()
    }

    pub fn get_row_template(&self) -> &Option<String> {
        &self.params.row_template
    }


    async fn load_items(&mut self, mut entities_to_load:Vec<String>) -> Result<(), String> {
        entities_to_load.sort() ;
        entities_to_load.dedup();
        match self.entities.load_entities(&self.wb_api, &entities_to_load).await {
            Ok(_) => {}
            Err(e) => return Err(format!("Error loading entities: {:?}", &e)),
        }
        Ok(())
    }

    fn gather_items_for_property(&mut self,prop:&str) -> Result<Vec<String>,String> {
        let mut entities_to_load = vec![];
        for row in self.results.iter() {
            if let Some(entity) = self.entities.get_entity(row.entity_id().to_owned()) {
                self.get_filtered_claims(&entity,prop)
                //entity.claims()
                    .iter()
                    .filter(|statement|statement.property()==prop)
                    .map(|statement|statement.main_snak())
                    .filter(|snak|*snak.datatype()==SnakDataType::WikibaseItem)
                    .filter_map(|snak|snak.data_value().to_owned())
                    .map(|datavalue|datavalue.value().to_owned())
                    .filter_map(|value|match value {
                        wikibase::value::Value::Entity(v) => Some(v.id().to_owned()),
                        _ => None
                    })
                    .for_each(|id|entities_to_load.push(id));
            }
        }
        Ok(entities_to_load)
    }

    fn gather_items_section(&mut self) -> Result<Vec<String>,String> {
        // TODO support all of SectionType
        let prop = match &self.params.section {
            SectionType::Property(p) => p.clone() ,
            SectionType::SparqlVariable(_v) => return Err("SPARQL variable section type not supported yet".to_string()),
            SectionType::None => return Ok(vec![]) // Nothing to do
        } ;
        self.gather_items_for_property(&prop)
    }

    fn gather_items_sort(&mut self) -> Result<Vec<String>, String> {
        let prop = match &self.params.sort {
            SortMode::Property(prop) => prop.clone(),
            _ => return Ok(vec![])
        };
        self.gather_items_for_property(&prop)
    }

    async fn gather_and_load_items(&mut self) -> Result<(), String> {
        // Gather items to load
        let mut entities_to_load : Vec<String> = vec![];
        for row in self.results.iter() {
            for cell in row.cells() {
                self.gather_entities_and_external_properties(cell.parts())
                    .iter()
                    .for_each(|entity_id|entities_to_load.push(entity_id.to_string()));
            }
        }
        if let SortMode::Property(prop) = &self.params.sort {
            entities_to_load.push(prop.to_string());
        }

        match &self.params.section {
            SectionType::Property(prop) => { entities_to_load.push(prop.to_owned()); }
            SectionType::SparqlVariable(_v) => return Err("SPARQL variable section type not supported yet".to_string()),
            SectionType::None => {}
        }
        self.load_items(entities_to_load).await?;

        entities_to_load = self.gather_items_sort()?;
        let mut v2 = self.gather_items_section()? ;
        entities_to_load.append(&mut v2);
        self.load_items(entities_to_load).await
    }


    fn gather_entities_and_external_properties(&self,parts:&[ResultCellPart]) -> Vec<String> {
        let mut entities_to_load = vec![];
        for part in parts {
            match part {
                ResultCellPart::Entity((item, true)) => {
                    entities_to_load.push(item.to_owned());
                }
                ResultCellPart::ExternalId((property, _id)) => {
                    entities_to_load.push(property.to_owned());
                }
                ResultCellPart::SnakList(v) => {
                    self.gather_entities_and_external_properties(&v)
                        .iter()
                        .for_each(|entity_id|entities_to_load.push(entity_id.to_string()))
                }
                _ => {}
            }
        }
        entities_to_load
    }

    pub fn column(&self,column_id:usize) -> Option<&Column> {
        self.columns.get(column_id)
    }

    pub fn skip_table(&self) -> bool {
        self.params.skip_table
    }

    pub fn get_section_ids(&self) -> Vec<usize> {
        let mut ret : Vec<usize> = self
            .results
            .iter()
            .map(|row|{row.section()})
            .collect();
        ret.sort_unstable();
        ret.dedup();
        ret
    }

    pub fn wiki(&self) -> &String {
        &self.page_params.wiki
    }

    pub fn page_title(&self) -> &String {
        &self.page_params.page
    }

    pub fn summary(&self) -> &Option<String> {
        &self.params.summary
    }

    pub fn header_template(&self) -> &Option<String> {
        &self.params.header_template
    }

    pub fn get_label_with_fallback(&self,entity_id:&str) -> String {
        match self.get_entity(entity_id) {
            Some(entity) => {
                match entity.label_in_locale(self.language()).map(|s|s.to_string()) {
                    Some(s) => s,
                    None => {
                        entity_id.to_string() // Fallback
                        /*
                        // Fallback to en
                        match entity.label_in_locale(self.default_language()).map(|s|s.to_string()) {
                            Some(s) => s,
                            None => entity_id.to_string()
                        }
                        */
                    }
                }
            }
            None => entity_id.to_string() // Fallback
        }
    }

    pub fn default_language(&self) -> &str {
        &self.page_params.config.default_language()
    }

}