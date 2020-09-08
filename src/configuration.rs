use crate::*;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum NamespaceGroup {
    All, // All namespaces forbidden
    List(Vec<i64>), // List of forbidden namespaces
}

impl NamespaceGroup {
    pub fn can_edit_namespace(&self,nsid: i64) -> bool {
        match self {
            Self::All => false ,
            Self::List(list) => nsid>=0 && !list.contains(&nsid)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Configuration {
    wb_apis: HashMap<String,Arc<Api>>,
    namespace_blocks: HashMap<String,NamespaceGroup>,
    default_api:String,
    prefer_preferred: bool,
    default_language: String,
    template_start_sites: HashMap<String,String>,
    template_end_sites: HashMap<String,String>,
    location_templates: HashMap<String,String>,
    shadow_images_check: Vec<String>,
    default_thumbnail_size: Option<u64>,
    location_regions: Vec<String>,
    mysql: Option<Value>,
    wiki_user: String,
    wiki_password: String,
}

impl Configuration {
    pub async fn new_from_file<P: AsRef<Path>>(path: P) -> Result<Self,String> {
        let file = File::open(path).map_err(|e|format!("{:?}",e))?;
        let reader = BufReader::new(file);
        let j = serde_json::from_reader(reader).map_err(|e|format!("{:?}",e))?;
        Self::new_from_json(j).await
    }

    pub async fn new_from_json ( j:Value ) -> Result<Self,String> {
        let mut ret : Self = Default::default();

        if let Some(s) = j["default_api"].as_str() { ret.default_api = s.to_string() }
        if let Some(s) = j["default_language"].as_str() { ret.default_language = s.to_string() }
        if let Some(b) = j["prefer_preferred"].as_bool() { ret.prefer_preferred = b }
        if let Some(i) = j["default_thumbnail_size"].as_u64() { ret.default_thumbnail_size = Some(i) }
        if let Some(sic) = j["shadow_images_check"].as_array() { ret.shadow_images_check = sic.iter().map(|s|s.as_str().unwrap().to_string()).collect() }
        if let Some(lr) = j["location_regions"].as_array() { ret.location_regions = lr.iter().map(|s|s.as_str().unwrap().to_string()).collect() }
        if let Some(s) = j["wiki_login"]["user"].as_str() { ret.wiki_user = s.to_string() }
        if let Some(s) = j["wiki_login"]["pass"].as_str() { ret.wiki_password = s.to_string() }
        if j["mysql"].is_object() {
            ret.mysql = Some(j["mysql"].to_owned()) ;
        }
        

        // valid WikiBase APIs
        if let Some(o) = j["apis"].as_object() {
            for (k,v) in o.iter() {
                if let (name,Some(url)) = (k.as_str(),v.as_str()) {
                    let api = wikibase::mediawiki::api::Api::new(&url).await.unwrap();
                    ret.wb_apis.insert(name.to_string(),Arc::new(api));
                }
                
            }
        }

        // Location template patterns
        if let Some(o) = j["location_templates"].as_object() {
            for (k,v) in o.iter() {
                if let (k,Some(v)) = (k.as_str(),v.as_str()) {
                    ret.location_templates.insert(k.to_string(),v.to_string());
                }
                
            }
        }

        // Namespace blocks on wikis
        if let Some(o) = j["namespace_blocks"].as_object() {
            for (k,v) in o.iter() {
                // Check for string value ("*")
                if let Some(s) = v.as_str() {
                    if s == "*" { // All namespaces
                        ret.namespace_blocks.insert(k.to_string(),NamespaceGroup::All);
                    } else {
                        return Err(format!("Unrecognized string value for namespace_blocks[{}]:{}",k,v));
                    }
                }

                // Check for array of integers
                if let Some(a) = v.as_array() {
                    let nsids : Vec<i64> = a.iter().filter_map(|v|v.as_u64()).map(|x|x as i64).collect();
                    ret.namespace_blocks.insert(k.to_string(),NamespaceGroup::List(nsids));
                }
            }
        }

        // Start/end template site/page mappings
        let api = ret.get_default_wbapi()?;
        let q_start = match j["template_start_q"].as_str() {
            Some(q) => q.to_string(),
            None => return Err("No template_start_q in config".to_string())
        } ;
        let q_end = match j["template_end_q"].as_str() {
            Some(q) => q.to_string(),//ret.template_end_sites = ret.get_template(q)?,
            None => return Err("No template_end_q in config".to_string())
        } ;
        let entities = wikibase::entity_container::EntityContainer::new();
        entities.load_entities(&api, &vec![q_start.clone(),q_end.clone()]).await.map_err(|e|e.to_string())?;
        ret.template_start_sites = ret.get_sitelink_mapping(&entities,&q_start)?;
        ret.template_end_sites = ret.get_sitelink_mapping(&entities,&q_end)?;

        Ok(ret)
    }

    pub fn wiki_user(&self) -> &String {
        &self.wiki_user
    }

    pub fn wiki_password(&self) -> &String {
        &self.wiki_password
    }

    pub fn mysql(&self,key: &str) -> Value {
        match &self.mysql {
            Some(mysql) => mysql[key].to_owned(),
            None => Value::Null,
        }
    }

    fn get_sitelink_mapping(&self, entities:&wikibase::entity_container::EntityContainer, q:&str ) -> Result<HashMap<String,String>,String> {
        let entity = entities.get_entity(q.to_owned()).ok_or(format!("Entity {} not found",&q))?;
        match entity.sitelinks() {
            Some(sl) => Ok(sl.iter().map(|s|(s.site().to_owned(),s.title().to_owned())).collect()),
            None => Err(format!("No sitelink in {}",&q))
        }
    }

    pub fn check_for_shadow_images(&self, wiki:&String) -> bool {
        self.shadow_images_check.contains(wiki)
    }

    pub fn get_local_template_title_start(&self,wiki: &str) -> Result<String,String> {
        let ret = self.template_start_sites.get(wiki).map(|s|s.to_string()).ok_or("Cannot find local start template".to_string())?;
        Ok(ret.split(':').last().unwrap().to_string())
    }

    pub fn get_local_template_title_end(&self,wiki: &str) -> Result<String,String> {
        let ret = self.template_end_sites.get(wiki).map(|s|s.to_string()).ok_or("Cannot find local end template".to_string())?;
        Ok(ret.split(':').last().unwrap().to_string())
    }

    pub fn can_edit_namespace(&self, wiki:&str, nsid:i64) -> bool {
        match self.namespace_blocks.get(wiki) {
            Some(nsg) => nsg.can_edit_namespace(nsid),
            None => true // Default
        }
    }

    pub fn get_location_template(&self, wiki:&str) -> String {
        self.location_templates.get(wiki).unwrap_or(self.location_templates.get(&"default".to_string()).unwrap()).to_string()
    }

    pub fn prefer_preferred(&self) -> bool {
        self.prefer_preferred
    }

    pub fn default_language(&self) -> &str {
        &self.default_language
    }

    pub fn default_thumbnail_size(&self) -> u64 {
        self.default_thumbnail_size.unwrap_or(128)
    }

    pub fn location_regions(&self) -> &Vec<String> {
        &self.location_regions
    }

    pub async fn wbapi_login(&mut self,key: &str) -> bool {
        match self.wb_apis.get_mut(key) {
            Some(mut api) => {
                (*Arc::get_mut(&mut api).unwrap()).login(self.wiki_user.to_owned(), self.wiki_password.to_owned()).await.expect("Could not log in");
                true
            }
            None => false
        }
    }

    pub fn get_wbapi(&self,key: &str) -> Option<&Arc<Api>> {
        self.wb_apis.get(key)
    }

    pub fn get_default_wbapi(&self) -> Result<&Arc<Api>,String> {
        self.wb_apis.get(&self.default_api).ok_or("No default API set in config file".to_string())
    }
}
