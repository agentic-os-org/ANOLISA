use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use crate::support::ui_approval_slice::{
    test_worker_profile, RealCoreApprovalHarness, SliceLaunch,
};

#[test]
fn candidate_first_code_observes_host_applied_isolation() {
    let root = tempfile::tempdir().expect("canary directory");
    let output = root.path().join("first-code.json");
    let candidate = root.path().join("candidate");
    let source = root.path().join("candidate.c");
    fs::write(&source, format!(r#"#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <dirent.h>
extern char **environ;
int main(void) {{
 int count=0, fds[4096], length=0, tty=open("/dev/tty",O_RDONLY);
 for(char **entry=environ; *entry; entry++) count++;
 if(tty>=0) close(tty);
 char cwd[4096], standard[3][4096]; getcwd(cwd,sizeof(cwd));
 for(int i=0;i<3;i++) {{ char path[32]; snprintf(path,sizeof(path),"/proc/self/fd/%d",i); ssize_t n=readlink(path,standard[i],4095); if(n<0) return 2; standard[i][n]=0; }}
 DIR *directory=opendir("/proc/self/fd"); if(!directory) return 3;
 struct dirent *entry; while((entry=readdir(directory))) {{ int fd=atoi(entry->d_name); if(entry->d_name[0]!='.' && fd!=dirfd(directory) && fcntl(fd,F_GETFD)>=0) fds[length++]=fd; }} closedir(directory);
 for(int i=0;i<length;i++) for(int j=i+1;j<length;j++) if(fds[j]<fds[i]) {{int temp=fds[i];fds[i]=fds[j];fds[j]=temp;}}
 FILE *out=fopen({:?},"w"); if(!out) return 4;
 fprintf(out,"{{\"env_count\":%d,\"cwd\":\"%s\",\"session_leader\":%s,\"tty\":%s,\"stdio\":[\"%s\",\"%s\",\"%s\"],\"fds\":[",count,cwd,getsid(0)==getpid()?"true":"false",tty>=0?"true":"false",standard[0],standard[1],standard[2]);
 for(int i=0;i<length;i++) fprintf(out,"%s%d",i?",":"",fds[i]); fputs("]}}",out); return fclose(out);
}}
"#, output.to_str().expect("UTF8 canary path"))).expect("write native canary");
    assert!(Command::new("cc")
        .arg("-o")
        .arg(&candidate)
        .arg(&source)
        .status()
        .expect("existing C compiler")
        .success());
    let profile = test_worker_profile(&candidate);
    let leak = fs::File::open(&candidate).expect("canary leaked descriptor");
    let fd = unsafe { nix::libc::fcntl(leak.as_raw_fd(), nix::libc::F_DUPFD, 40) };
    assert!(fd >= 40);
    assert_eq!(unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) }, 0);
    let _leaked = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        worker_profile: Some(profile.path().to_owned()),
        worker_binary: Some(profile.path().join("cosh-ui-worker")),
        ..SliceLaunch::activated()
    });
    assert!(
        harness.request_approval_card(),
        "probe exits without Ready; fallback stays visible"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(&output).expect("candidate first-code report"))
            .expect("canary JSON");
    assert_eq!(
        state,
        serde_json::json!({"env_count":0,"cwd":"/","session_leader":true,"tty":false,"fds":[0,1,2,3],"stdio":["/dev/null","/dev/null","/dev/null"]})
    );
    harness
        .approve_execute_shell_second_request_from_visible_card()
        .check_inline_baseline()
        .expect("fallback approval executes once with stable Bash");
}

#[test]
fn a_worker_outside_the_shell_profile_cannot_execute_its_first_code() {
    let root = tempfile::tempdir().expect("untrusted worker directory");
    let worker = root.path().join("cosh-ui-worker");
    let marker = root.path().join("executed");
    fs::write(
        &worker,
        format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    )
    .expect("write hostile candidate");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).expect("executable");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        worker_binary: Some(worker),
        ..SliceLaunch::activated()
    });
    assert!(
        harness.request_approval_card(),
        "refused candidate must leave inline approval"
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    assert!(
        !marker.exists(),
        "unpinned candidate executed before admission"
    );
    observation
        .check_inline_baseline()
        .expect("inline request executes once");
}
