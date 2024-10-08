extern crate config;
extern crate serde_json;

use config::{Config, File};
use listeria::configuration::Configuration;
use listeria::listeria_page::ListeriaPage;
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;

async fn update_page(settings: &Config, page_title: &str, api_url: &str) -> Result<String, String> {
    let config = Arc::new(Configuration::new_from_file("config.json").await.unwrap());

    let mut mw_api = wikibase::mediawiki::api::Api::new(api_url)
        .await
        .map_err(|e| e.to_string())?;

    let token = settings.get_string("user.token").expect("No oauth2 user.token");
    mw_api.set_oauth2(&token);

    let mw_api = Arc::new(RwLock::new(mw_api));
    let mut page = ListeriaPage::new(config, mw_api, page_title.into()).await?;
    page.run().await?;

    let message = match page.update_source_page().await? {
        true => format!("{} edited", &page_title),
        false => format!("{} not edited", &page_title),
    };

    Ok(message)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let ini_file = "listeria.ini";

    let settings = Config::builder()
        .add_source(File::new(ini_file, config::FileFormat::Ini))
        .build()
        .unwrap_or_else(|_| panic!("INI file '{}' can't be opened", ini_file));

    let args: Vec<String> = env::args().collect();
    let wiki_server = args
        .get(1)
        .ok_or_else(|| "No wiki server argument".to_string())?;
    let page = args.get(2).ok_or_else(|| "No page argument".to_string())?;

    let wiki_api = format!("https://{}/w/api.php", &wiki_server);
    let message = match update_page(&settings, &page, &wiki_api).await {
        Ok(m) => format!("OK: {}", m),
        Err(e) => format!("ERROR: {}", e),
    };
    println!("{}", message);
    Ok(())
}
