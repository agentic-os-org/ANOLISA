use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) struct OutputFault {
    _root: tempfile::TempDir,
    library: PathBuf,
}

impl OutputFault {
    pub(crate) fn build() -> Self {
        let root = tempfile::tempdir().expect("output interposer directory");
        let source = root.path().join("output_fault.c");
        let library = root.path().join("output_fault.so");
        std::fs::write(
            &source,
            r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>
static ssize_t (*real_write)(int,const void*,size_t);
static ssize_t (*real_writev)(int,const struct iovec*,int);
static int shell, triggered, recorded, withdraw, audit_committed, cup_state, staged;
static size_t matched, audit_matched;
static const char audit_needle[]="\"event_type\":\"approval.resolved\"";
__attribute__((constructor)) static void setup(void) {
 real_write=dlsym(RTLD_NEXT,"write"); real_writev=dlsym(RTLD_NEXT,"writev");
 char exe[4096]; ssize_t n=readlink("/proc/self/exe",exe,sizeof(exe)-1);
 if(n>0){exe[n]=0;char *base=strrchr(exe,'/');base=base?base+1:exe;
 shell=!strcmp(base,"cosh-shell")||!strcmp(base,"cosh-shell (deleted)");}
 const char *mode=getenv("COSH_UI_OUTPUT_TRIGGER"); withdraw=mode&&!strcmp(mode,"withdraw");
}
static void inspect_audit(const void *data,size_t size) {
 const char *bytes=data;
 for(size_t i=0;i<size&&!audit_committed;i++) {
  if(bytes[i]==audit_needle[audit_matched])audit_matched++;
  else audit_matched=bytes[i]=='"'?1:0;
  if(audit_matched==sizeof(audit_needle)-1)audit_committed=1;
 }
}
static void inspect(const void *data,size_t size) {
 const char *bytes=data,*needle="Approved";
 for(size_t i=0;i<size&&!triggered;i++){
  if(!withdraw){
   if(bytes[i]==needle[matched])matched++;else matched=bytes[i]=='A'?1:0;
   if(matched==8)triggered=1;
  }else{
   unsigned char ch=bytes[i];
   if(cup_state==0)cup_state=ch==27?1:0;
   else if(cup_state==1)cup_state=ch=='['?2:0;
   else if(cup_state==2){if(ch==';')cup_state=3;else if(ch<'0'||ch>'9')cup_state=0;}
   else {if(ch=='H'){if(audit_committed)triggered=1;cup_state=0;}else if(ch<'0'||ch>'9')cup_state=0;}
  }
 }
}
static int failed(void) {
 const char *arm=getenv("COSH_UI_OUTPUT_ARM");
 if(!shell||!triggered||!arm||access(arm,F_OK))return 0;
 if(!recorded){const char *hit=getenv("COSH_UI_OUTPUT_HIT");
 if(hit){int fd=open(hit,O_WRONLY|O_CREAT|O_CLOEXEC,0600);
 if(fd>=0){const char *reason=withdraw?"EIO after CUP\n":"EIO after Approved\n";real_write(fd,reason,strlen(reason));close(fd);}}
 recorded=1;}
 errno=EIO;return 1;
}
static void pause_stage(int fd) {
 if(!shell||!withdraw||!triggered||staged||fd==1)return;
 char link[64],path[4096];snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
 ssize_t n=readlink(link,path,sizeof(path)-1);if(n<=0)return;path[n]=0;
 char *base=strrchr(path,'/');base=base?base+1:path;
 if(strcmp(base,"shell-handoff-request"))return;
 staged=1;const char *hit=getenv("COSH_UI_STAGE_HIT"),*release=getenv("COSH_UI_STAGE_RELEASE");
 if(hit){int marker=open(hit,O_WRONLY|O_CREAT|O_CLOEXEC,0600);
  if(marker>=0){real_write(marker,"actual sidecar write paused\n",27);close(marker);}}
 for(int i=0;release&&access(release,F_OK)&&i<1000;i++)usleep(10000);
}
ssize_t write(int fd,const void *data,size_t size) {
 if(shell&&fd==1){inspect(data,size);if(failed())return -1;}
 pause_stage(fd);
 ssize_t written=real_write(fd,data,size);
 if(shell&&withdraw&&fd!=1&&written>0)inspect_audit(data,(size_t)written);
 return written;
}
ssize_t writev(int fd,const struct iovec *iov,int count) {
 if(shell&&fd==1){for(int i=0;i<count;i++)inspect(iov[i].iov_base,iov[i].iov_len);
 if(failed())return -1;}
 pause_stage(fd);
 ssize_t written=real_writev(fd,iov,count);
 if(shell&&withdraw&&fd!=1&&written>0){size_t remaining=(size_t)written;
  for(int i=0;i<count&&remaining;i++){size_t n=iov[i].iov_len<remaining?iov[i].iov_len:remaining;
   inspect_audit(iov[i].iov_base,n);remaining-=n;}}
 return written;
}
"#,
        )
        .expect("interposer source");
        let output = Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .arg("-ldl")
            .output()
            .expect("existing cc");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            _root: root,
            library,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.library
    }
}
