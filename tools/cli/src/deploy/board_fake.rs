//! The board deploy against a fake SSH: shell scripts standing in for `ssh`
//! and `scp`, playing a board whose home directory is a scratch directory on
//! this machine: `ssh` runs its remote command there with `SSH_CONNECTION`
//! set, `scp` copies into it. The whole sequence — opening the session,
//! copying the app and its assets, launching it with the dev variables,
//! closing it with a signal, staging a restart — runs on any Unix host.

use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viso_project::DeployAddress;

use super::board::Board;
use super::*;

/// `ssh [-o opt]... [-p port] dest -- command`: the command, run by `sh` in
/// the board's home; 255 with SSH's complaint while `$STATE/down` exists.
const SSH: &str = r#"#!/bin/sh
STATE="__STATE__"
echo "ssh $*" >> "$STATE/calls"
while [ "$#" -gt 0 ]; do
    case "$1" in
    -o|-p) shift 2 ;;
    --) shift; break ;;
    *) shift ;;
    esac
done
if [ -f "$STATE/down" ]; then
    echo "ssh: connect to host board port 22: Connection refused" >&2
    exit 255
fi
cd "$STATE/home" || exit 255
SSH_CONNECTION="127.0.0.1 50022 127.0.0.1 22" exec sh -c "$*"
"#;

/// `scp [-q] [-o opt]... [-r] [-P port] from host:to`: a copy into the
/// board's home.
const SCP: &str = r#"#!/bin/sh
STATE="__STATE__"
echo "scp $*" >> "$STATE/calls"
recursive=""
while [ "$#" -gt 2 ]; do
    case "$1" in
    -o|-P) shift 2 ;;
    -r) recursive="-R"; shift ;;
    *) shift ;;
    esac
done
to="$STATE/home/${2#*:}"
exec cp $recursive "$1" "$to"
"#;

/// A fake board, its state directory and the project it deploys.
struct Fake {
    dir: PathBuf,
    state: PathBuf,
    ssh: PathBuf,
    scp: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("viso-board-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = dir.join("state");
        std::fs::create_dir_all(state.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("project/assets")).unwrap();
        std::fs::write(dir.join("project/assets/logo.txt"), b"logo").unwrap();
        let ssh = tool(&dir, &state, "ssh", SSH);
        let scp = tool(&dir, &state, "scp", SCP);
        Fake {
            dir,
            state,
            ssh,
            scp,
        }
    }

    fn board(&self, address: &str) -> Board {
        Board::with_tools(
            self.ssh.clone().into(),
            self.scp.clone().into(),
            DeployAddress::parse(address).unwrap(),
            "probe",
            Some(self.dir.join("project/assets")),
        )
    }

    /// An executable "app": `body` run by `sh`.
    fn app(&self, name: &str, body: &str) -> PathBuf {
        let path = self.dir.join("build").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The session directory on the board.
    fn session(&self) -> PathBuf {
        self.state.join("home/viso-dev/probe")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tool(dir: &Path, state: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        script.replace("__STATE__", &state.display().to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn vars() -> Vars<'static> {
    Vars {
        address: "127.0.0.1:4242".into(),
        token: "t0k",
        session: "5e55",
        build_id: "b111d",
        env: &[],
    }
}

/// How `app` ended, waiting at most five seconds.
fn end_of(app: &mut App) -> Ended {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ended) = app.try_wait().expect("the connection runs") {
            return ended;
        }
        assert!(Instant::now() < deadline, "the app never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every line `app` wrote to stdout, once it ended.
fn said(app: &mut App) -> Vec<String> {
    let (out, lines) = app.outputs().into_iter().next().expect("its stdout");
    assert_eq!(lines, Lines::Out);
    BufReader::new(out).lines().map_while(Result::ok).collect()
}

#[test]
fn a_session_opens_copies_and_launches_the_app_with_the_dev_variables() {
    let fake = Fake::new("session");
    std::fs::create_dir_all(fake.session()).unwrap();
    std::fs::write(fake.session().join("stale"), b"old").unwrap();
    let board = fake.board("root@127.0.0.1:2222");
    // The route to the board leaves from its loopback.
    assert_eq!(board.open(), Ok(IpAddr::from([127, 0, 0, 1])));
    assert!(fake.session().is_dir() && !fake.session().join("stale").exists());

    let exe = fake.app(
        "probe",
        "echo \"runtime=$VISO_DEV_RUNTIME token=$VISO_DEV_TOKEN restore=$VISO_DEV_RESTORE\"\n\
         echo \"args=$*\"\necho \"cwd=$(basename \"$PWD\") logo=$(cat assets/logo.txt)\"\nexit 7",
    );
    let mut deploy = Deploy::Board(Box::new(board));
    let args = ["--level".into(), "it's 2".into()];
    let mut app = deploy
        .launch(&exe, &args, &vars(), true, None, true)
        .expect("it launches");
    let lines = said(&mut app);
    assert_eq!(
        lines,
        [
            "runtime=127.0.0.1:4242 token=t0k restore=1",
            "args=--level it's 2",
            "cwd=probe logo=logo",
        ]
    );
    // The connection's status is the app's.
    let ended = end_of(&mut app);
    assert!(!ended.success());
    assert_eq!(ended.to_string(), "exit status: 7");
    let calls = fake.calls();
    assert!(
        calls
            .iter()
            .all(|c| !c.starts_with("ssh ") || c.contains("-p 2222 root@127.0.0.1 --")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("scp ")
            && c.contains("-P 2222")
            && c.ends_with(":viso-dev/probe/probe")),
        "{calls:?}"
    );
}

#[test]
fn a_close_reaches_the_app_on_the_board_and_a_restart_moves_the_staged_build() {
    let fake = Fake::new("close");
    let board = fake.board("pi@127.0.0.1");
    board.open().unwrap();
    let mut deploy = Deploy::Board(Box::new(board));
    let exe = fake.app(
        "probe",
        "echo ready\ntrap 'echo closing; exit 0' TERM\nwhile :; do sleep 0.05; done",
    );
    let mut app = deploy
        .launch(&exe, &[], &vars(), false, None, true)
        .expect("it launches");
    let (out, _) = app.outputs().into_iter().next().unwrap();
    let mut out = BufReader::new(out).lines();
    assert_eq!(out.next().unwrap().unwrap(), "ready");
    assert!(app.try_wait().unwrap().is_none(), "it runs");
    app.close();
    let ended = end_of(&mut app);
    assert!(ended.success(), "{ended}");
    assert_eq!(out.next().unwrap().unwrap(), "closing");

    // The new build is copied beside the old one while it runs, and moved
    // into place at the launch: no second copy.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&exe, "#!/bin/sh\necho second build\n").unwrap();
    let mut staging = deploy.prewarm(&exe, &[]).expect("a board stages").unwrap();
    assert!(staging.wait().unwrap().success());
    assert!(fake.session().join(".next-probe").is_file());
    let copies = |fake: &Fake| {
        fake.calls()
            .iter()
            .filter(|c| c.starts_with("scp "))
            .count()
    };
    let before = copies(&fake);
    let mut again = deploy
        .launch(&exe, &[], &vars(), true, None, true)
        .expect("it relaunches");
    assert_eq!(said(&mut again), ["second build"]);
    assert!(end_of(&mut again).success());
    assert_eq!(copies(&fake), before, "{:?}", fake.calls());
    assert!(!fake.session().join(".next-probe").exists());
}

#[test]
fn a_board_that_cannot_be_reached_says_why() {
    let fake = Fake::new("down");
    std::fs::write(fake.state.join("down"), b"").unwrap();
    let why = fake.board("root@127.0.0.1").open().unwrap_err();
    assert!(why.contains("`ssh` failed"), "{why}");
    assert!(why.contains("Connection refused"), "{why}");

    // A connection that fails under the app ends it as a failure of the
    // connection, not of the app.
    std::fs::remove_file(fake.state.join("down")).unwrap();
    let board = fake.board("root@127.0.0.1");
    board.open().unwrap();
    let exe = fake.app("probe", "exit 0");
    let mut deploy = Deploy::Board(Box::new(board));
    std::fs::write(fake.state.join("down"), b"").unwrap();
    let mut app = deploy
        .launch(&exe, &[], &vars(), false, None, true)
        .expect("the connection starts");
    let ended = end_of(&mut app);
    assert!(!ended.success());
    assert_eq!(
        ended.to_string(),
        "the SSH connection to the board failed or was lost"
    );
}
