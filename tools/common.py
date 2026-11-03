"""Development tooling only. No AI orchestration loop lives here."""
from __future__ import annotations
import hashlib, json, os, signal, subprocess, tempfile, time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence
ROOT = Path(__file__).resolve().parents[1]
def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat()
def sha256(path: Path) -> str:
    h=hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda:f.read(1024*1024),b''): h.update(b)
    return h.hexdigest()
def load_json(path: Path) -> Any:
    with path.open(encoding='utf-8') as f: return json.load(f)
def atomic_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True,exist_ok=True)
    fd,tmp=tempfile.mkstemp(prefix='.'+path.name+'.',dir=path.parent)
    try:
        with os.fdopen(fd,'w',encoding='utf-8') as f:
            json.dump(data,f,ensure_ascii=False,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
        os.replace(tmp,path)
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
def execute(argv: Sequence[str],cwd: Path,out: Path,err: Path,timeout: float,input_data: bytes|None=None) -> tuple[int,bool,float]:
    if not argv or timeout<=0: raise ValueError('command and positive timeout required')
    out.parent.mkdir(parents=True,exist_ok=True);err.parent.mkdir(parents=True,exist_ok=True)
    start=time.monotonic();timed=False
    with out.open('wb') as fo,err.open('wb') as fe:
        p=subprocess.Popen(list(argv),cwd=str(cwd),stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                           stdout=fo,stderr=fe,start_new_session=True)
        try:
            p.communicate(input=input_data,timeout=timeout)
        except subprocess.TimeoutExpired:
            timed=True;terminate_group(p)
        except BaseException:
            terminate_group(p);raise
        return (124 if timed else int(p.returncode)),timed,time.monotonic()-start
