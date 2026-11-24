use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all="camelCase")]
pub struct SharedFile { pub name:String, pub size:u64, pub token:String, pub url:String }

pub fn dir(project: &Path) -> PathBuf { project.join(".aiddiction-files") }
pub fn share(project: &Path, source: &str, name: Option<&str>, base_url: &str) -> Result<SharedFile,String> {
 let src=Path::new(source); if !src.is_file(){return Err("file does not exist".into())}
 let clean=name.unwrap_or_else(||src.file_name().and_then(|x|x.to_str()).unwrap_or("file"));
 if clean.is_empty() || clean.contains('/') || clean.contains('\\') || clean=="." || clean==".." {return Err("invalid file name".into())}
 let d=dir(project); std::fs::create_dir_all(&d).map_err(|e|e.to_string())?;
 let token=Uuid::new_v4().to_string(); let target=d.join(format!("{token}-{clean}")); std::fs::copy(src,&target).map_err(|e|e.to_string())?;
 let size=std::fs::metadata(&target).map_err(|e|e.to_string())?.len();
 Ok(SharedFile{name:clean.into(),size,token:token.clone(),url:format!("{base_url}/files/{token}")})
}
pub fn list(project:&Path, base_url:&str)->Vec<SharedFile>{
 let Ok(entries)=std::fs::read_dir(dir(project)) else{return vec![]}; entries.flatten().filter_map(|e|{let n=e.file_name().to_string_lossy().into_owned(); let (token,name)=n.split_once('-')?; let size=e.metadata().ok()?.len();Some(SharedFile{name:name.into(),size,token:token.into(),url:format!("{base_url}/files/{token}")})}).collect()
}
pub fn find(root:&Path, token:&str)->Option<(PathBuf,String)>{ if token.len()<8 || token.contains('/') {return None}; std::fs::read_dir(root).ok()?.flatten().find_map(|e|{let n=e.file_name().to_string_lossy().into_owned(); let (t,name)=n.split_once('-')?; (t==token).then_some((e.path(),name.into()))}) }
