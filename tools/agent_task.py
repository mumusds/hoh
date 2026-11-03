#!/usr/bin/env python3
"""One native CLI invocation for bootstrapping HoH. No retries or supervisor loop.

Audit only: a native turn completing never means the product passed acceptance.
"""
from __future__ import annotations
import argparse, fcntl, json, os, shutil, subprocess, sys, uuid
from pathlib import Path
from common import ROOT, Terminated, atomic_json, execute, flag_supported, install_termination_handlers, load_json, restore_handlers, running_as_root, sha256, utcnow

# Set in every native child. A nested bridge call with --top-level is refused,
# so a delegated or already-running top-level session cannot start another commander.
PARENT_ENV='HOH_BOOTSTRAP_PARENT'
TOP_LEVEL_ROLES={'commander','developer'}

REQUIRED={
 'codex':['--json','--dangerously-bypass-approvals-and-sandbox'],
 'claude':['--output-format','--verbose','--dangerously-skip-permissions'],
 'pi':['--mode','--session-dir'],
}
def help_text(agent:str,cwd:Path)->str:
    cmd=[agent,'exec','--help'] if agent=='codex' else [agent,'--help']
    r=subprocess.run(cmd,cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20,check=False)
    if r.returncode:raise RuntimeError(f'{agent} help failed ({r.returncode})')
    return r.stdout.decode('utf-8',errors='replace')

def command_for(agent:str,session_dir:Path,model:str|None,help_output:str|None=None)->list[str]:
    if help_output is not None:
        missing=[x for x in REQUIRED[agent] if not flag_supported(help_output,x)]
        if missing:raise ValueError('unsupported native CLI flags: '+','.join(missing))
    if agent=='codex':cmd=['codex','exec','--json','--dangerously-bypass-approvals-and-sandbox','-']
    elif agent=='claude':cmd=['claude','-p','--dangerously-skip-permissions','--output-format','stream-json','--verbose']
    else:
        cmd=['pi','--mode','json','--session-dir',str(session_dir)]
        if help_output and flag_supported(help_output,'--approve'):cmd+=['--approve']
    if model:
        # Keep stdin marker last for Codex; never shell-interpolate the model name.
        if agent=='codex':cmd[-1:-1]=['--model',model]
        else:cmd+=['--model',model]
    return cmd

def inspect_output(agent:str,path:Path)->dict:
    records=0;malformed=0;terminal=False;failed=False;session_ids=set();assistant_text=[];models=set()
    with path.open('rb') as f:
        while True:
            line=f.readline(1024*1024+1)
            if not line:break
            if len(line)>1024*1024:
                malformed+=1
                while line and not line.endswith(b'\n'):line=f.readline(1024*1024+1)
                continue
            if not line.strip():continue
            try:r=json.loads(line)
            except (ValueError,UnicodeError):malformed+=1;continue
            if not isinstance(r,dict):malformed+=1;continue
            records+=1;t=r.get('type','')
            text=None
            if agent=='codex' and t=='item.completed':
                item=r.get('item',{})
                if isinstance(item,dict) and item.get('type')=='agent_message':text=item.get('text')
            elif agent=='claude' and t=='result':text=r.get('result')
            elif agent=='pi' and t=='message_end':
                message=r.get('message',{})
                if isinstance(message,dict) and message.get('role')=='assistant':
                    content=message.get('content',[])
                    if isinstance(content,list):text=''.join(b.get('text','') for b in content if isinstance(b,dict) and b.get('type')=='text')
            if isinstance(text,str):
                assistant_text.append(text[-4096:]);assistant_text=assistant_text[-16:]
            for k in ['session_id','sessionId','thread_id']:
                if isinstance(r.get(k),str):session_ids.add(r[k])
            if isinstance(r.get('model'),str):models.add(r['model'])
            if agent=='codex':
                terminal|=t=='turn.completed'
                failed|=t in ['turn.failed','error']
            elif agent=='claude' and t=='result':
                terminal=True;failed|=r.get('is_error',False) or r.get('subtype') not in [None,'success']
            elif agent=='pi':
                # JSON mode is a one-shot process: observed process exit is also
                # required by main. Production RPC MUST use negotiated settled.
                terminal|=t in ['agent_settled','agent_end']
                failed|=t in ['error','agent_error']
                msg=r.get('message',{})
                if isinstance(msg,dict):failed|=msg.get('stopReason') in ['error','aborted']
    return {'records':records,'malformed_records':malformed,'terminal_observed':terminal,'native_error_observed':failed,'session_ids':sorted(session_ids),'resolved_models':sorted(models),'assistant_text_tail':'\n'.join(assistant_text)[-8192:]}

def cli_version(agent:str,cwd:Path)->str:
    try:
        r=subprocess.run([agent,'--version'],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20,check=False)
        text=r.stdout.decode('utf-8','replace').strip().splitlines()
        return text[0][:160] if r.returncode==0 and text else 'unknown'
    except (OSError,subprocess.TimeoutExpired):return 'unknown'

def record_live_sessions(agent:str,version:str,observed:dict)->None:
    """When run inside tools/run_check.py, append observed native sessions to the check's live-sessions.json."""
    directory=os.getenv('HOH_EVIDENCE_DIR');check=os.getenv('HOH_CHECK_ID');candidate=os.getenv('HOH_CANDIDATE_ID')
    if not (directory and check and candidate) or not observed['session_ids']:return
    path=Path(directory)/'live-sessions.json'
    with (Path(directory)/'.live-sessions.lock').open('a+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        data=load_json(path) if path.exists() else {'schema_version':'1.0','check_id':check,'harness_candidate_id':candidate,'sessions':[]}
        model=observed['resolved_models'][0] if len(observed['resolved_models'])==1 else 'unknown'
        for sid in observed['session_ids']:data['sessions'].append({'agent':agent,'cli_version':version,'session_id':sid,'resolved_model':model})
        atomic_json(path,data)



def main(argv=None)->int:
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--agent',choices=list(REQUIRED),required=True)
    p.add_argument('--role',default='developer')
    p.add_argument('--prompt-file',type=Path,required=True)
    p.add_argument('--cwd',type=Path,default=ROOT)
    p.add_argument('--output-dir',type=Path)
    p.add_argument('--model')
    p.add_argument('--timeout',type=float,default=float(os.getenv('HOH_CHILD_TIMEOUT_SECONDS','3600')))
    p.add_argument('--dry-run',action='store_true')
    p.add_argument('--top-level',action='store_true',help='Only for the initial commander launched by start.sh; never use for delegated work.')
    a=p.parse_args(argv)
    if a.timeout<=0 or not a.prompt_file.is_file() or not a.cwd.is_dir():p.error('positive timeout, existing prompt and cwd required')
    original=a.prompt_file.read_bytes()
    if not original.strip():p.error('prompt must not be empty')
    if a.top_level and a.role not in TOP_LEVEL_ROLES:p.error('--top-level requires role commander or developer')
    if a.top_level and os.getenv(PARENT_ENV):p.error(f'--top-level refused: already inside bootstrap session {os.getenv(PARENT_ENV)}; a nested session cannot start another top-level coordinator')
    if a.top_level:data=b'[HOH_TOP_LEVEL_BOOTSTRAP]\n'+original
    else:
        envelope=('[HOH_DELEGATED_TASK]\nRole: '+a.role+'\nYou are a scoped worker, NOT the top-level project commander. Execute only the task below. Do not start LAUNCH_PROMPT.md, bootstrap the whole project, advance the global taskboard, or recursively create another commander. Return a scoped result and stop. Project invariants still apply.\n\n')
        data=envelope.encode('utf-8')+original
    attempt_id=str(uuid.uuid4())
    # Inside run_check, attempts live in the check's evidence directory so the gate can prove provenance.
    base=Path(os.environ['HOH_EVIDENCE_DIR'])/'attempts' if os.getenv('HOH_EVIDENCE_DIR') else ROOT/'.hoh-dev'/'attempts'
    out=(a.output_dir or base/attempt_id).resolve()
    model=a.model or os.getenv('HOH_MODEL_'+a.agent.upper()) or None
    if a.dry_run:
        print(json.dumps({'dry_run':True,'capabilities_validated':False,'cwd':str(a.cwd.resolve()),'argv':command_for(a.agent,out/'sessions',model),'prompt_bytes':len(data)},ensure_ascii=False,indent=2));return 0
    if out.exists():print(f'Attempt directory already exists: {out}; attempts are append-only.',file=sys.stderr);return 64
    if not shutil.which(a.agent):print(f'Missing native CLI: {a.agent}',file=sys.stderr);return 127
    if running_as_root():
        print('Native agents run with bypass/YOLO flags; use the VM non-root development user, not root.',file=sys.stderr);return 78
    try:help_output=help_text(a.agent,a.cwd);cmd=command_for(a.agent,out/'sessions',model,help_output)
    except (RuntimeError,ValueError,subprocess.TimeoutExpired) as exc:print(str(exc),file=sys.stderr);return 78
    if a.agent=='codex':
        git=subprocess.run(['git','rev-parse','--show-toplevel'],cwd=a.cwd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        if git.returncode:
            if '--skip-git-repo-check' not in help_output:print('Codex requires a Git workspace.',file=sys.stderr);return 78
            cmd.insert(-1,'--skip-git-repo-check')
    out.mkdir(parents=True,exist_ok=False)
    (out/'sessions').mkdir()
    (out/'prompt.md').write_bytes(data)
    (out/'native-help.txt').write_text(help_output,encoding='utf-8')
    version=cli_version(a.agent,a.cwd)
    record={'schema_version':'1.1','record_type':'bootstrap_native_attempt','attempt_id':attempt_id,'candidate_id':os.getenv('HOH_CANDIDATE_ID'),'check_id':os.getenv('HOH_CHECK_ID'),'parent_attempt':os.getenv(PARENT_ENV),'agent':a.agent,'cli_version':version,'role':a.role,'argv':cmd,'cwd':str(a.cwd.resolve()),'started_at':utcnow(),'prompt_sha256':sha256(out/'prompt.md'),'model_requested':model,'top_level_bootstrap':a.top_level,'status':'RUNNING','product_accepted':False}
    atomic_json(out/'attempt.json',record)
    print(f'Native {a.agent} invocation; logs: {out}',file=sys.stderr,flush=True)
    previous=install_termination_handlers()
    try:
        rc,timed,dur=execute(cmd,a.cwd.resolve(),out/'stdout.jsonl',out/'stderr.log',a.timeout,data,env=os.environ|{PARENT_ENV:attempt_id})
        observed=inspect_output(a.agent,out/'stdout.jsonl')
        ok=rc==0 and observed['terminal_observed'] and not observed['native_error_observed'] and observed['malformed_records']==0
        record.update(observed,exit_code=rc,timed_out=timed,duration_seconds=dur,finished_at=utcnow(),status='NATIVE_TURN_COMPLETED' if ok else 'NATIVE_FAILED',stdout_sha256=sha256(out/'stdout.jsonl'),stderr_sha256=sha256(out/'stderr.log'))
        atomic_json(out/'attempt.json',record)
        if ok:record_live_sessions(a.agent,version,observed)
        print(json.dumps({'attempt_record':str(out/'attempt.json'),'native_completed':ok,'product_accepted':False},ensure_ascii=False))
        return 0 if ok else (rc if 1<=rc<=125 else 1)
    except (KeyboardInterrupt,Terminated) as exc:
        record.update(status='INTERRUPTED',finished_at=utcnow(),error=type(exc).__name__);atomic_json(out/'attempt.json',record);return 130
    except Exception as exc:
        record.update(status='BRIDGE_FAILED',finished_at=utcnow(),error=f'{type(exc).__name__}: {exc}');atomic_json(out/'attempt.json',record)
        print(str(exc),file=sys.stderr);return 1
    finally:restore_handlers(previous)
if __name__=='__main__':sys.exit(main())
