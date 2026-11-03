import sys, paramiko

HOST, PORT, USER, PASS = "45.205.27.41", 22, "root", "xxlnKUDX2993"


def connect():
    cli = paramiko.SSHClient()
    cli.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    cli.connect(HOST, port=PORT, username=USER, password=PASS,
                timeout=30, banner_timeout=30, auth_timeout=30,
                look_for_keys=False, allow_agent=False)
    return cli


METHOD = """    /// Number of TCP sockets currently held by this tunnel's smoltcp socket set.
    ///
    /// Diagnostic only: the stress harness uses this to detect socket leaks
    /// that /proc/<pid>/fd cannot see, since smoltcp sockets are heap state.
    pub async fn socket_count(&self) -> usize {
        self.stack.socket_count().await
    }
}
"""


def main():
    path = "/root/warpgate-rs/app/crates/wg-tunnel/src/connectip/native.rs"
    cli = connect()
    sftp = cli.open_sftp()
    with sftp.open(path, "rb") as f:
        data = f.read().decode("utf-8")
    nl = "\r\n" if "\r\n" in data else "\n"
    if "pub async fn socket_count" in data:
        print("already patched")
    else:
        anchor = "fn split_target(target: &str)"
        i = data.index(anchor)
        # find the "}\n\n" that closes the impl block just before the anchor
        j = data.rindex("}" + nl + nl, 0, i)
        new = data[:j] + METHOD + data[i:]
        with sftp.open("/tmp/native.rs.pre_diag", "wb") as f:
            f.write(data.encode("utf-8"))
        with sftp.open(path, "wb") as f:
            f.write(new.encode("utf-8"))
        print("patched", len(data), "->", len(new))
    with sftp.open(path, "rb") as f:
        out = f.read().decode("utf-8")
    for n, line in enumerate(out.splitlines(), 1):
        if "socket_count" in line or "fn split_target" in line:
            print(n, line)
    sftp.close()
    cli.close()


if __name__ == "__main__":
    main()
