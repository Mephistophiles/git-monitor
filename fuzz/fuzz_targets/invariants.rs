#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if data.len()>16_384{return;}
    let (old,new)=data.split_at(data.len()/2);
    let document=git_monitor::document::DiffDocument::build("scope",old,new,1,1,10000);
    assert_eq!(document.reconstruct(old,new).unwrap(),new);
    let config=git_monitor::config::DiffConfig::default();
    let limits=git_monitor::config::Limits::default();
    let _=git_monitor::diff::patch_rows(data,&config,&limits);
    let escaped=git_monitor::render::escape(&String::from_utf8_lossy(data));
    assert!(!escaped.contains('<'));
    let file=git_monitor::diff::build("scope.txt",old,new,1,1,None,&config,&limits);
    file.validate().unwrap();
    let _=git_monitor::split::bisect(&git_monitor::split::begin(&file));
});
