# subprocess: run other programs.
import _subprocess;

PIPE = -1;
STDOUT = -2;
DEVNULL = -3;

class SubprocessError(Exception) { pass; }

class CalledProcessError(SubprocessError) {
    def init(self, returncode, cmd, output = None, stderr = None) {
        SubprocessError.init(self, f"Command {repr(cmd)} returned non-zero exit status {returncode}.");
        self.returncode = returncode;
        self.cmd = cmd;
        self.output = output;
        self.stdout = output;
        self.stderr = stderr;
    }
}

class TimeoutExpired(SubprocessError) {
    def init(self, cmd, timeout, output = None, stderr = None) {
        SubprocessError.init(self, f"Command {repr(cmd)} timed out after {timeout} seconds");
        self.cmd = cmd;
        self.timeout = timeout;
        self.output = output;
        self.stdout = output;
        self.stderr = stderr;
    }
}

class CompletedProcess {
    def init(self, args, returncode, stdout = None, stderr = None) {
        self.args = args;
        self.returncode = returncode;
        self.stdout = stdout;
        self.stderr = stderr;
    }
    def check_returncode(self) {
        if (self.returncode != 0) {
            raise CalledProcessError(self.returncode, self.args, self.stdout, self.stderr);
        }
    }
    def __repr__(self) {
        let s = f"CompletedProcess(args={repr(self.args)}, returncode={self.returncode}";
        if (self.stdout is not None) { s += f", stdout={repr(self.stdout)}"; }
        if (self.stderr is not None) { s += f", stderr={repr(self.stderr)}"; }
        return s + ")";
    }
}

# run(["git", "status"], capture_output=True) -> CompletedProcess
# Output is text (UTF-8, "\r\n" turned into "\n").
def run(args, capture_output = False, check = False, cwd = None, shell = False, input = None,
    timeout = None, env = None, stdout = None, stderr = None, text = True) {
    let cap_out = capture_output or stdout == PIPE;
    let cap_err = capture_output or stderr == PIPE;
    let merge = stderr == STDOUT;
    let r = _subprocess.run(args, shell, cwd, input, cap_out, cap_err, merge, timeout, env,
        stdout == DEVNULL, stderr == DEVNULL);
    if (r[3]) { raise TimeoutExpired(args, timeout, r[1], r[2]); }
    let done = CompletedProcess(args, r[0], r[1], r[2]);
    if (check) { done.check_returncode(); }
    return done;
}

# The program's output; raises CalledProcessError if it fails.
def check_output(args, **kwargs) {
    kwargs["stdout"] = PIPE;
    kwargs["check"] = True;
    return run(args, **kwargs).stdout;
}

# Runs the program and returns its exit code.
def call(args, **kwargs) { return run(args, **kwargs).returncode; }

def check_call(args, **kwargs) {
    kwargs["check"] = True;
    run(args, **kwargs);
    return 0;
}

# Output of a shell command, without the trailing newline.
def getoutput(cmd) {
    let r = run(cmd, shell=True, stdout=PIPE, stderr=STDOUT);
    return r.stdout.rstrip("\n");
}

def getstatusoutput(cmd) {
    let r = run(cmd, shell=True, stdout=PIPE, stderr=STDOUT);
    return (r.returncode, r.stdout.rstrip("\n"));
}
