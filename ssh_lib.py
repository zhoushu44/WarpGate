import sys, paramiko, io, os

HOST = "45.205.27.41"
PORT = 22
USER = "root"
PASS = "xxlnKUDX2993"


def connect():
    cli = paramiko.SSHClient()
    cli.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    cli.connect(HOST, port=PORT, username=USER, password=PASS,
                timeout=30, banner_timeout=30, auth_timeout=30,
                look_for_keys=False, allow_agent=False)
    return cli


def run(cli, cmd, timeout=3600, pty=False):
    tr = cli.get_transport()
    tr.set_keepalive(15)
    ch = tr.open_session()
    if pty:
        ch.get_pty()
    ch.settimeout(timeout)
    ch.exec_command(cmd)
    out = io.BytesIO()
    while True:
        try:
            data = ch.recv(65536)
        except Exception as e:
            sys.stderr.write("\n[ssh read error: %s]\n" % e)
            break
        if not data:
            break
        out.write(data)
    try:
        code = ch.recv_exit_status()
    except Exception:
        code = -1
    return out.getvalue(), code


def run_cmd(cmd, timeout=3600, pty=False):
    cli = connect()
    try:
        out, code = run(cli, cmd, timeout, pty)
        sys.stdout.buffer.write(out)
        sys.stdout.flush()
        sys.stderr.write("\n[exit code: %s]\n" % code)
        return code
    finally:
        cli.close()


def put(local, remote):
    cli = connect()
    try:
        sftp = cli.open_sftp()
        sftp.put(local, remote)
        sftp.chmod(remote, 0o755)
        sftp.close()
    finally:
        cli.close()


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "put":
        put(sys.argv[2], sys.argv[3])
        print("uploaded %s -> %s" % (sys.argv[2], sys.argv[3]))
    else:
        cmd = sys.argv[1]
        timeout = int(sys.argv[2]) if len(sys.argv) > 2 else 3600
        pty = len(sys.argv) > 3 and sys.argv[3] == "pty"
        sys.exit(run_cmd(cmd, timeout, pty))
