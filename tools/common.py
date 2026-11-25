"""Development tooling only. No AI orchestration loop lives here."""
from __future__ import annotations
import hashlib, json, os, re, signal, subprocess, tempfile, time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence
ROOT = Path(__file__).resolve().parents[1]
# Generated/third-party trees that kit validators must never scan or hash.
EXCLUDED_DIRS = frozenset({'.git','.hoh','.hoh-dev','.venv','venv','node_modules','target','dist','__pycache__'})
def is_excluded(path: Path, root: Path) -> bool:
    return any(part in EXCLUDED_DIRS for part in path.relative_to(root).parts)
def flag_supported(help_text: str, flag: str) -> bool:
    """Whole-token match so '--mode' is not satisfied by '--model'."""
    return re.search(r'(?<![\w-])'+re.escape(flag)+r'(?![\w-])',help_text) is not None
def running_as_root() -> bool:
    return hasattr(os,'geteuid') and os.geteuid()==0
def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat()
def sha256(path: Path) -> str:
    h=hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda:f.read(1024*1024),b''): h.update(b)
    return h.hexdigest()
def load_json(path: Path) -> Any:
    with path.open(encoding='utf-8') as f: return json.load(f)
def atomic_json(path: Path, data: Any, exclusive: bool=False) -> None:
    """Write via temp file + fsync. exclusive=True fails atomically if path exists."""
    path.parent.mkdir(parents=True,exist_ok=True)
    fd,tmp=tempfile.mkstemp(prefix='.'+path.name+'.',dir=path.parent)
    try:
        with os.fdopen(fd,'w',encoding='utf-8') as f:
            json.dump(data,f,ensure_ascii=False,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
        if exclusive:os.link(tmp,path)  # raises FileExistsError; never overwrites
        else:os.replace(tmp,path)
        dfd=os.open(path.parent,os.O_DIRECTORY)
        try: os.fsync(dfd)
        finally: os.close(dfd)
    finally:
        if os.path.exists(tmp):os.unlink(tmp)
def safe_path(root: Path, rel: str) -> Path:
    if not isinstance(rel,str) or not rel: raise ValueError('empty artifact path')
    if Path(rel).is_absolute(): raise ValueError('artifact path must be relative')
    p=(root/rel).resolve()
    if not p.is_relative_to(root.resolve()): raise ValueError('artifact escapes bundle/project root')
    return p
def ref_for(path: Path, root: Path=ROOT) -> dict[str,str]:
    p=path.resolve()
    return {'path':str(p.relative_to(root.resolve())),'sha256':sha256(p)}
def terminate_group(proc: subprocess.Popen, grace: float=2.0) -> None:
    """Terminate the owned process group, including children ignoring SIGTERM."""
    try:os.killpg(proc.pid,signal.SIGTERM)
    except ProcessLookupError:pass
    deadline=time.monotonic()+grace
    while time.monotonic()<deadline:
        proc.poll()
        try:os.killpg(proc.pid,0)
        except ProcessLookupError:break
        time.sleep(0.02)
    try:os.killpg(proc.pid,signal.SIGKILL)
    except ProcessLookupError:pass
    proc.wait(timeout=max(grace,1.0))
class Terminated(Exception):
    """Termination observed by execute polling, or by the legacy protocol recorder's own lifecycle."""

class SignalRecord(dict):
    """Previous handlers plus every observed signal in delivery order, retained until main restores them."""
    def __init__(self):
        super().__init__();self.received=[];self.parent=None
    def handler(self,signum,frame):
        self.received.append(signum)
    def poll(self):
        if self.received:raise Terminated(', '.join(signal.Signals(s).name for s in self.received))

_signal_record=None

def install_termination_handlers() -> dict:
    """Legacy recorder only: its own Proc registry and signal masking handle exception-based cancellation.

    execute callers must use install_interrupt_handlers instead; changing this entry would break the recorder's
    separate lifecycle (tests/bundle/test_record.py).
    """
    def handler(signum,frame):raise Terminated(signal.Signals(signum).name)
    return {s:signal.signal(s,handler) for s in (signal.SIGTERM,signal.SIGHUP)}

def install_interrupt_handlers() -> SignalRecord:
    global _signal_record
    record=SignalRecord();record.parent=_signal_record;_signal_record=record
    try:
        for signum in (signal.SIGINT,signal.SIGTERM,signal.SIGHUP):
            previous=signal.getsignal(signum)
            # M3LS-200: a background job without job control inherits SIGINT ignored; keep it so (unrecorded).
            if signum==signal.SIGINT and previous==signal.SIG_IGN:continue
            record[signum]=previous
            signal.signal(signum,record.handler)
    except BaseException:
        restore_handlers(record)
        raise
    return record

def restore_handlers(previous: dict) -> None:
    global _signal_record
    for s,h in previous.items():signal.signal(s,h)
    if isinstance(previous,SignalRecord):_signal_record=previous.parent

def execute(argv: Sequence[str],cwd: Path,out: Path,err: Path,timeout: float,input_data: bytes|None=None,env: dict[str,str]|None=None,grace: float=2.0,on_spawn: Callable[[],None]|None=None) -> tuple[int,bool,float]:
    if not argv or timeout<=0: raise ValueError('command and positive timeout required')
    signals=_signal_record
    def interrupted():
        if signals is not None:signals.poll()
    out.parent.mkdir(parents=True,exist_ok=True);err.parent.mkdir(parents=True,exist_ok=True)
    start=time.monotonic();timed=False
    with out.open('wb') as fo,err.open('wb') as fe:
        p=None
        try:
            interrupted() # Before spawn: no child to clean up, started stays false.
            try:
                p=subprocess.Popen(list(argv),cwd=str(cwd),stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                                   stdout=fo,stderr=fe,start_new_session=True,env=env)
            except BaseException:
                interrupted() # A signal during a failed spawn is also NOT_STARTED.
                raise
            # No handler raises, so a successful Popen always reaches this notification and the cleanup owner.
            if on_spawn is not None:on_spawn()
            interrupted()
            deadline=time.monotonic()+timeout;data=input_data
            while True:
                interrupted()
                remaining=deadline-time.monotonic()
                if remaining<=0:
                    timed=True;terminate_group(p,grace);interrupted();break
                try:
                    p.communicate(input=data,timeout=min(0.05,remaining))
                    interrupted()
                    break
                except subprocess.TimeoutExpired:
                    # communicate retains unsent input across retries; resubmitting it is an error.
                    data=None
        except BaseException:
            if p is not None:terminate_group(p,grace)
            raise
        return (124 if timed else int(p.returncode)),timed,time.monotonic()-start
