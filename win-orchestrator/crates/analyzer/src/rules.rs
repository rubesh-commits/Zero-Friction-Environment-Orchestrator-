use winorchestrator_core::Language;
use winorchestrator_core::Version;

/// One detected version-sensitive construct.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuleHit {
    pub language: Language,
    pub construct: String,
    pub minimum: Version,
    pub explanation: String,
    pub file: String,
    pub line: usize,
    pub snippet: String,
}

impl RuleHit {
    pub fn new(
        language: Language,
        construct: &str,
        minimum: Version,
        explanation: &str,
        file: &str,
        line: usize,
        snippet: &str,
    ) -> Self {
        let snippet = snippet
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(100)
            .collect();
        Self {
            language,
            construct: construct.to_string(),
            minimum,
            explanation: explanation.to_string(),
            file: file.to_string(),
            line,
            snippet,
        }
    }
}

/// Python standard library module names. Keeping this embedded makes the
/// analyzer independent of whichever Python (if any) is installed on the host.
pub fn python_is_stdlib(module: &str) -> bool {
    let name = module.split('.').next().unwrap_or(module);
    if name.starts_with('_') {
        return true;
    }
    matches!(
        name,
        "abc"
            | "aifc"
            | "argparse"
            | "array"
            | "ast"
            | "asyncio"
            | "atexit"
            | "audioop"
            | "base64"
            | "bdb"
            | "binascii"
            | "bisect"
            | "builtins"
            | "bz2"
            | "cProfile"
            | "calendar"
            | "cmath"
            | "cmd"
            | "code"
            | "codecs"
            | "codeop"
            | "collections"
            | "colorsys"
            | "compileall"
            | "concurrent"
            | "configparser"
            | "contextlib"
            | "contextvars"
            | "copy"
            | "copyreg"
            | "csv"
            | "ctypes"
            | "curses"
            | "dataclasses"
            | "datetime"
            | "dbm"
            | "decimal"
            | "difflib"
            | "dis"
            | "doctest"
            | "email"
            | "encodings"
            | "ensurepip"
            | "enum"
            | "errno"
            | "faulthandler"
            | "fcntl"
            | "filecmp"
            | "fileinput"
            | "fnmatch"
            | "fractions"
            | "ftplib"
            | "functools"
            | "gc"
            | "getopt"
            | "getpass"
            | "gettext"
            | "glob"
            | "graphlib"
            | "grp"
            | "gzip"
            | "hashlib"
            | "heapq"
            | "hmac"
            | "html"
            | "http"
            | "idlelib"
            | "imaplib"
            | "importlib"
            | "inspect"
            | "io"
            | "ipaddress"
            | "itertools"
            | "json"
            | "keyword"
            | "lib2to3"
            | "linecache"
            | "locale"
            | "logging"
            | "lzma"
            | "mailbox"
            | "mailcap"
            | "marshal"
            | "math"
            | "mimetypes"
            | "mmap"
            | "modulefinder"
            | "msilib"
            | "msvcrt"
            | "multiprocessing"
            | "netrc"
            | "nis"
            | "nntplib"
            | "numbers"
            | "operator"
            | "optparse"
            | "os"
            | "ossaudiodev"
            | "pathlib"
            | "pdb"
            | "pickle"
            | "pickletools"
            | "pipes"
            | "pkgutil"
            | "platform"
            | "plistlib"
            | "poplib"
            | "posix"
            | "posixpath"
            | "pprint"
            | "profile"
            | "pstats"
            | "pty"
            | "pwd"
            | "py_compile"
            | "pyclbr"
            | "pydoc"
            | "queue"
            | "quopri"
            | "random"
            | "re"
            | "readline"
            | "reprlib"
            | "resource"
            | "rlcompleter"
            | "runpy"
            | "sched"
            | "secrets"
            | "select"
            | "selectors"
            | "shelve"
            | "shlex"
            | "shutil"
            | "signal"
            | "site"
            | "smtplib"
            | "sndhdr"
            | "socket"
            | "socketserver"
            | "spwd"
            | "sqlite3"
            | "sre_compile"
            | "sre_constants"
            | "sre_parse"
            | "ssl"
            | "stat"
            | "statistics"
            | "string"
            | "stringprep"
            | "struct"
            | "subprocess"
            | "sunau"
            | "symtable"
            | "sys"
            | "sysconfig"
            | "syslog"
            | "tabnanny"
            | "tarfile"
            | "telnetlib"
            | "tempfile"
            | "termios"
            | "textwrap"
            | "threading"
            | "time"
            | "timeit"
            | "tkinter"
            | "token"
            | "tokenize"
            | "tomllib"
            | "trace"
            | "traceback"
            | "tracemalloc"
            | "tty"
            | "turtle"
            | "turtledemo"
            | "types"
            | "typing"
            | "unicodedata"
            | "unittest"
            | "urllib"
            | "uu"
            | "uuid"
            | "venv"
            | "warnings"
            | "wave"
            | "weakref"
            | "webbrowser"
            | "winreg"
            | "winsound"
            | "wsgiref"
            | "xdrlib"
            | "xml"
            | "xmlrpc"
            | "zipapp"
            | "zipfile"
            | "zipimport"
            | "zlib"
            | "zoneinfo"
    )
}

/// Modules whose very presence requires a newer Python interpreter.
pub fn python_import_rule(module: &str) -> Option<(&'static str, Version, &'static str)> {
    match module.split('.').next()? {
        "tomllib" => Some((
            "tomllib import",
            Version::new(3, 11, 0),
            "The tomllib module was added in Python 3.11.",
        )),
        "zoneinfo" => Some((
            "zoneinfo import",
            Version::new(3, 9, 0),
            "zoneinfo was added in Python 3.9.",
        )),
        "graphlib" => Some((
            "graphlib import",
            Version::new(3, 9, 0),
            "graphlib was added in Python 3.9.",
        )),
        _ => None,
    }
}

/// JavaScript/TypeScript modules provided by the Node runtime.
pub fn node_is_builtin(module: &str) -> bool {
    let name = module
        .trim_start_matches("node:")
        .split('/')
        .next()
        .unwrap_or(module);
    name.starts_with("node:")
        || matches!(
            name,
            "assert"
                | "async_hooks"
                | "buffer"
                | "child_process"
                | "cluster"
                | "console"
                | "constants"
                | "crypto"
                | "dgram"
                | "diagnostics_channel"
                | "dns"
                | "domain"
                | "events"
                | "fs"
                | "http"
                | "http2"
                | "https"
                | "inspector"
                | "module"
                | "net"
                | "os"
                | "path"
                | "perf_hooks"
                | "process"
                | "punycode"
                | "querystring"
                | "readline"
                | "repl"
                | "stream"
                | "string_decoder"
                | "test"
                | "timers"
                | "tls"
                | "trace_events"
                | "tty"
                | "url"
                | "util"
                | "v8"
                | "vm"
                | "wasi"
                | "worker_threads"
                | "zlib"
                | "sea"
        )
}

pub fn java_is_stdlib(module: &str) -> bool {
    module.starts_with("java.")
        || module.starts_with("javax.")
        || module.starts_with("jdk.")
        || module.starts_with("sun.")
        || module.starts_with("com.sun.")
}

/// Java APIs that imply a language/API level. Only unambiguous, high-signal
/// patterns are listed to avoid false positives.
pub fn java_import_rule(module: &str) -> Option<(&'static str, Version, &'static str)> {
    if module.starts_with("java.net.http") {
        return Some((
            "java.net.http",
            Version::new(11, 0, 0),
            "The java.net.http client was added in Java 11.",
        ));
    }
    if module.starts_with("java.util.random") {
        return Some((
            "java.util.random",
            Version::new(17, 0, 0),
            "The java.util.random package was added in Java 17.",
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdlib_filters_are_sane() {
        assert!(python_is_stdlib("typing"));
        assert!(python_is_stdlib("importlib.metadata"));
        assert!(!python_is_stdlib("requests"));
        assert!(node_is_builtin("node:fs"));
        assert!(node_is_builtin("path"));
        assert!(!node_is_builtin("@scope/pkg"));
        assert!(java_is_stdlib("java.util.List"));
        assert!(!java_is_stdlib("org.springframework"));
    }
}
