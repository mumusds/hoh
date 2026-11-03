import contextlib,io,json,os,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
from helpers import ROOT,load_json
from jsonschema import Draft202012Validator
from validate_bundle import validate
import taskboard

class BundleContracts(unittest.TestCase):
    def test_whole_bundle(self):
        r=validate(ROOT);self.assertEqual(r['status'],'PASS',r['errors']);self.assertFalse(r['product_accepted'])
    def test_protocol_examples(self):
        v=Draft202012Validator(load_json(ROOT/'contracts/protocol.schema.json'))
        for file in ['context-handoff.json','hcp-message.json','command.json']:
            with self.subTest(file=file):v.validate(load_json(ROOT/'examples'/file))
    def test_protocol_rejects_extra_field(self):
        v=Draft202012Validator(load_json(ROOT/'contracts/protocol.schema.json'));e=load_json(ROOT/'examples/command.json');e['invented_field']=1
        self.assertFalse(v.is_valid(e))
    def test_total_release_scope(self):
        r=load_json(ROOT/'contracts/requirements.json')['requirements'];self.assertEqual(len(r),70);self.assertTrue(all(x['required_release'] for x in r))
    def test_live_auth_does_not_block_offline_core(self):
        tasks={t['id']:t for t in load_json(ROOT/'contracts/development-tasks.json')['tasks']}
        def ancestors(i):
            seen=set();stack=list(tasks[i]['depends_on'])
            while stack:
                t=stack.pop()
                if t not in seen:seen.add(t);stack.extend(tasks[t]['depends_on'])
            return seen
        for t in tasks:
            if t.startswith(('M1-','M2-')):self.assertNotIn('M0-T04',ancestors(t),t)
        plugin=next(t for t in tasks.values() if 'REQ-048' in t['requirements'] and t['kind']=='implement')
        self.assertIn('M0-T04',plugin['depends_on'])
    def test_release_classes_and_interim_scope(self):
        r=load_json(ROOT/'contracts/requirements.json')['requirements']
        self.assertEqual({x['id'] for x in r if x['release_class']=='degradable'},{'REQ-048','REQ-049','REQ-050'})
        self.assertTrue(all('v1' in x['release_targets'] for x in r))
        self.assertTrue(all(x['spec_maturity']=='detailed' for x in r if x['milestone'] in ('M1','M2')))
    def test_unimplemented_checks_fail(self):
        plan=load_json(ROOT/'tests/check-plan.json')
        for cid,entry in plan['checks'].items():
            if entry['argv'][-3:-2]==['scripts/product/not-implemented.sh'] or 'not-implemented.sh' in ' '.join(entry['argv']):
                with self.subTest(check=cid):
                    p=subprocess.run(entry['argv'],cwd=ROOT,capture_output=True,text=True,timeout=30)
                    self.assertEqual(p.returncode,3);self.assertIn('NOT_IMPLEMENTED',p.stderr)
    def test_trivial_plan_command_rejected(self):
        import validate_bundle
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            for argv,message in [(['true'],'always-succeeding'),(['env','X=1','true'],'always-succeeding'),(['bash','-c','exit 0'],'inline code'),
                                 (['python3','-c','pass'],'inline code'),(['sh','-c',':'],'inline code'),(['env','bash','-c','exit 0'],'inline code'),
                                 (['env','python3','-c','pass'],'inline code'),(['env','-u','X','true'],'always-succeeding'),(['sh','-ec',':'],'inline code'),
                                 (['bash','-lc','true'],'inline code'),(['python3','-Ic','pass'],'inline code'),(['env','-S','true'],'split-string')]:
                plan=load_json(ROOT/'tests/check-plan.json');plan['checks']['recovery']['argv']=argv
                (root/'tests/check-plan.json').write_text(json.dumps(plan))
                r=validate_bundle.validate(root)
                with self.subTest(argv=argv):self.assertTrue(any(message in e for e in r['errors']),r['errors'])
    def test_plan_command_guard_handles_operands(self):
        # M0R3-04: env's '-' form, options with separate or attached operands and --eval= bypassed the guard.
        from validate_bundle import plan_command_problem as problem
        for argv,message in [(['env','-','true'],'always-succeeding'),(['env','-i','true'],'always-succeeding'),
                             (['env','--ignore-environment','X=1','true'],'always-succeeding'),(['env','-','bash','-c','exit 0'],'inline code'),
                             (['bash','-o','errexit','-c','exit 0'],'inline code'),(['bash','+o','posix','-c','true'],'inline code'),
                             (['bash','-O','extglob','-c','true'],'inline code'),(['python3','-W','ignore','-c','pass'],'inline code'),
                             (['python3','-X','dev','-c','pass'],'inline code'),(['python3','-Wignore','-c','pass'],'inline code'),
                             (['node','--eval=0'],'inline code'),(['node','--print=1'],'inline code'),(['node','-r','x','-e','0'],'inline code'),
                             (['node','--require','x','--eval','0'],'inline code')]:
            with self.subTest(argv=argv):self.assertIn(message,problem(argv) or '')
        # Committed scripts stay allowed, also behind options whose values contain the inline letters.
        for argv in [['bash','scripts/product/check.sh','unit'],['bash','-o','pipefail','scripts/x.sh','-c'],['env','-','PATH=/usr/bin','bash','scripts/x.sh'],
                     ['python3','-W','default::DeprecationWarning','tools/x.py'],['python3','-Wdefault::DeprecationWarning','tools/x.py','-c'],
                     ['python3','-m','unittest','discover'],['node','-r','./setup.js','scripts/x.mjs'],['node','--input-type=module','scripts/x.mjs']]:
            with self.subTest(argv=argv):self.assertIsNone(problem(argv))
    def test_compat_manifest_schema_matches_ac_027_01(self):
        # CR3R-04: the schema forbade the field names AC-027-01 required.
        import validate_bundle
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            reqs=load_json(root/'contracts/requirements.json')
            ac=next(a for r in reqs['requirements'] for a in r['acceptance'] if a['id']=='AC-027-01')
            ac['then']='每项含 path、binary_sha256（等于已知值）、version 原文、help_sha256、schema_sha256（无 schema 为 null）'
            (root/'contracts/requirements.json').write_text(json.dumps(reqs,ensure_ascii=False))
            ex=load_json(root/'examples/compat-manifest.json');del ex['agents']['codex']['transport']
            (root/'examples/compat-manifest.json').write_text(json.dumps(ex))
            errors=validate_bundle.validate(root)['errors']
            self.assertTrue(any('AC-027-01 must name' in e and 'launcher_sha256' in e and 'transport' in e for e in errors),errors)
            self.assertTrue(any(e.startswith('example compat-manifest.json') and 'transport' in e for e in errors),errors)
    def test_scenario_message_kind_matches_its_ac(self):
        # CR7R-01: TEST-021-01 asserted kind=dispatch while AC-021-01 requires kind=handoff.
        import validate_bundle
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            path=root/'contracts/acceptance-scenarios.json';original=path.read_text()
            # CR8R-04: values with dots, digits or capitals must be compared whole, not by a lowercase prefix.
            for sid,old,new in [('TEST-021-01','kind=handoff','kind=dispatch'),('TEST-021-01','kind=handoff','kind=handoff2'),
                                ('TEST-004-02','kind=run.result','kind=run.started'),('TEST-001-02','kind=StartRun','kind=CancelRun')]:
                doc=json.loads(original);s=next(x for x in doc['scenarios'] if x['id']==sid)
                text=json.dumps(s,ensure_ascii=False);self.assertIn(old,text)
                doc['scenarios'][doc['scenarios'].index(s)]=json.loads(text.replace(old,new))
                path.write_text(json.dumps(doc,ensure_ascii=False))
                errors=validate_bundle.validate(root)['errors']
                self.assertTrue(any(e.startswith(sid+':') and new in e for e in errors),(sid,new,errors))
    def test_original_skills_available(self):
        skills=load_json(ROOT/'resources/skills/registry.json')['skills'];self.assertEqual(len(skills),13)
        for s in skills:self.assertTrue((ROOT/s['entry']).is_file())
        # Every role of the default team has at least one built-in skill (CR-003 added research for researcher).
        roles={r for s in skills for r in s['roles']}
        self.assertLessEqual({'commander','cto','developer','qa','researcher','reviewer'},roles)
    def test_bootstrap_agent_entries(self):
        for d in ['.agents','.claude','.pi']:self.assertTrue((ROOT/d/'skills/hoh-bootstrap/SKILL.md').exists())
    def test_product_check_does_not_accept_empty_product(self):
        # Valid only while this is a design kit. A completed implementation has its own tests.
        if (ROOT/'Cargo.toml').exists():return
        p=subprocess.run(['bash',str(ROOT/'scripts/check.sh'),'--product'],capture_output=True,text=True)
        self.assertEqual(p.returncode,3);self.assertIn('NOT_IMPLEMENTED',p.stderr)
    def test_missing_manifest_fails_closed(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/release_gate.py'),'--manifest',str(ROOT/'never-existed.json')],capture_output=True,text=True)
        self.assertEqual(p.returncode,1);self.assertEqual(json.loads(p.stderr)['status'],'FAIL')

class ValidatorScope(unittest.TestCase):
    def test_product_files_are_not_kit_files(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            (root/'web').mkdir();(root/'web/tsconfig.json').write_text('{ // comments are valid here\n}\n')
            (root/'crates/core').mkdir(parents=True);(root/'crates/core/README.md').write_text('```rust\n/// see [Foo](crate::Foo)\n```\n')
            (root/'tests/fixtures').mkdir(parents=True,exist_ok=True);(root/'tests/fixtures/broken.json').write_text('{not json')
            r=validate(root);self.assertEqual(r['status'],'PASS',r['errors'])

class SetupAndLedger(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh 包 path ');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)/'project';self.root.mkdir()
        for d in ['scripts','tools','config','contracts','examples']:shutil.copytree(ROOT/d,self.root/d,ignore=shutil.ignore_patterns('__pycache__'))
        for f in ['LAUNCH_PROMPT.md','requirements-dev.txt']:shutil.copy(ROOT/f,self.root/f)
    def shell(self,*args,env=None):
        e={k:v for k,v in os.environ.items() if k!='HOH_BOOTSTRAP_PARENT'}|(env or {})
        return subprocess.run(['bash',str(self.root/'scripts'/args[0]),*args[1:]],capture_output=True,text=True,timeout=30,env=e)
    def test_bootstrap_requires_yolo_ack(self):self.assertEqual(self.shell('bootstrap.sh','--dry-run').returncode,64)
    def test_bootstrap_dry_no_writes(self):
        before=set(self.root.rglob('*'));p=self.shell('bootstrap.sh','--yolo','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(set(self.root.rglob('*')),before)
    def test_unknown_bootstrap_option(self):self.assertEqual(self.shell('bootstrap.sh','--yolo','--made-up').returncode,64)
    def test_offline_setup_idempotent(self):
        p=self.shell('bootstrap.sh','--yolo','--skip-install');self.assertIn(p.returncode,[0,3],p.stderr)
        prog=self.root/'.hoh-dev/PROGRESS.md';self.assertTrue(prog.exists());prog.write_text('KEEP_EXISTING_PROGRESS')
        p=self.shell('bootstrap.sh','--yolo','--skip-install');self.assertIn(p.returncode,[0,3],p.stderr);self.assertEqual(prog.read_text(),'KEEP_EXISTING_PROGRESS')
        self.assertTrue((self.root/'.hoh-dev/env.sh').is_file())
    def test_start_interactive_dry(self):
        p=self.shell('start.sh','codex','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertIn('--dangerously-bypass',p.stdout);self.assertIn('HOH_TOP_LEVEL_BOOTSTRAP',p.stdout)
    def test_start_defaults_to_main_developer(self):
        p=self.shell('start.sh','--headless','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(json.loads(p.stdout)['argv'][0],'claude')
    def test_start_refuses_nested_top_level(self):
        p=self.shell('start.sh','--dry-run',env={'HOH_BOOTSTRAP_PARENT':'outer'});self.assertEqual(p.returncode,78)
    def test_start_pi_offline_with_verified_packages(self):
        # M0R3-05: the interactive Pi launch runs offline and refuses unverified project packages.
        (self.root/'.pi').mkdir();(self.root/'.pi/settings.json').write_text(json.dumps({'packages':['npm:pi-demo@1.0.0']}))
        p=self.shell('start.sh','pi','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertIn('PI_OFFLINE=1',p.stdout)
        (self.root/'.pi/npm/node_modules/pi-demo').mkdir(parents=True)
        (self.root/'.pi/npm/package-lock.json').write_text(json.dumps({'packages':{'node_modules/pi-demo':{'version':'1.0.0','integrity':'sha512-TAMPERED'}}}))
        p=self.shell('start.sh','pi','--dry-run');self.assertEqual(p.returncode,78);self.assertIn('pi-demo',p.stderr)
    def test_start_headless_dry(self):
        p=self.shell('start.sh','pi','--headless','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertTrue(json.loads(p.stdout)['dry_run'])
    def test_plugin_plan_is_dry(self):
        p=subprocess.run([sys.executable,str(self.root/'tools/install_pi_plugins.py'),'--dry-run'],capture_output=True,text=True);self.assertEqual(p.returncode,0,p.stderr);self.assertIn('@juicesharp/rpiv-todo',p.stdout);self.assertFalse((self.root/'.hoh-dev').exists())
    def test_plugin_integrity_mismatch_degrades(self):
        fake=self.root/'fakebin';fake.mkdir()
        (fake/'pi').write_text('#!/bin/sh\ncase "$*" in *--help*) echo "--local --approve";; *--version*) echo 1.5.0;; *) exit 0;; esac\n')
        (fake/'npm').write_text('#!/bin/sh\ncase "$*" in *dist.integrity*) echo sha512-TAMPERED;; *) echo 1.0.0;; esac\n')
        for f in fake.iterdir():f.chmod(0o755)
        p=subprocess.run([sys.executable,str(self.root/'tools/install_pi_plugins.py')],capture_output=True,text=True,
                         env=os.environ|{'PATH':str(fake)+os.pathsep+os.environ['PATH'],'PYTHONDONTWRITEBYTECODE':'1'})
        self.assertEqual(p.returncode,3,p.stderr);lock=load_json(self.root/'.hoh-dev/plugin-lock.json')
        self.assertTrue(all(x['state']=='DEGRADED' and 'differs from the pinned' in x['error'] for x in lock['plugins'].values()),lock)
    def ledger(self,*args):
        with patch.object(taskboard,'ROOT',self.root),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(io.StringIO()):return taskboard.main(list(args))
    def refused(self,*args):
        with self.assertRaises(SystemExit):self.ledger(*args)
    def seed(self,done):
        (self.root/'.hoh-dev').mkdir(exist_ok=True)
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':{t:{'task_id':t,'status':'done','owner':'seed','note':'TEST seed'} for t in done},'history':[]}))
    def state(self,t):return load_json(self.root/'.hoh-dev/task-state.json')['tasks'][t]
    def test_ledger_dependency_reject(self):self.refused('claim','M0-T02','--owner','dev')
    def test_ledger_running_only_via_claim(self):self.refused('set','M0-T01','running')
    def test_ledger_claim_needs_owner(self):self.refused('claim','M0-T01')
    def test_ledger_done_needs_note(self):
        self.ledger('claim','M0-T01','--owner','dev');self.refused('set','M0-T01','done','--owner','dev')
    def test_ledger_only_owner_finishes(self):
        self.ledger('claim','M0-T01','--owner','dev');self.refused('set','M0-T01','done','--owner','someone-else','--note','x')
    def test_ledger_illegal_transition(self):self.refused('set','M0-T01','done','--note','skipped running')
    def test_ledger_progress_and_dependencies(self):
        self.assertEqual(self.ledger('claim','M0-T01','--owner','dev'),0)
        self.assertEqual(self.ledger('set','M0-T01','done','--owner','dev','--note','TEST fixture only'),0)
        self.assertEqual(self.ledger('claim','M0-T02','--owner','dev'),0)
        state=load_json(self.root/'.hoh-dev/task-state.json');self.assertEqual(len(state['history']),3);self.assertEqual(self.state('M0-T02')['owner'],'dev')
    def refused_with(self,text,*args):
        err=io.StringIO()
        with patch.object(taskboard,'ROOT',self.root),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(err),self.assertRaises(SystemExit):taskboard.main(list(args))
        self.assertIn(text,err.getvalue())
    def test_ledger_single_writer(self):
        self.seed(['M0-T01','M0-T02','M0-T03','M1-T01'])
        self.assertEqual(self.ledger('claim','M1-T02','--owner','a'),0)
        self.refused_with('single writer','claim','M2-T01','--owner','b')  # a writer whose dependencies are done
        self.assertEqual(self.ledger('claim','M0-T04','--owner','c'),0)  # non-writer research runs in parallel
    def test_ledger_single_writer_across_divergent_worktree_plans(self):
        # CR3R-01: a frozen checkout must see a writer claimed from a newer plan it does not know.
        main=self.root;git=lambda *a:subprocess.run(['git','-C',str(main),*a],check=True,capture_output=True)
        for cmd in (['init','-q'],['config','user.email','t@invalid.example'],['config','user.name','t'],['add','-A'],['commit','-q','-m','x'],['worktree','add','-q',str(self.root.parent/'frozen')]):git(*cmd)
        frozen=self.root.parent/'frozen'
        def add_task(root,tid):
            f=root/'contracts/development-tasks.json';d=load_json(f)
            d['tasks'].append({'id':tid,'milestone':'M0','kind':'research','writer':True,'title':tid,'requirements':[],'depends_on':[],'done_when':'x'})
            f.write_text(json.dumps(d,ensure_ascii=False))
        add_task(main,'NEW-WRITER');add_task(frozen,'OLD-WRITER')
        self.assertEqual(self.ledger('claim','NEW-WRITER','--owner','a'),0)
        self.assertTrue(self.state('NEW-WRITER')['writer'])
        self.root=frozen
        try:
            self.refused_with('NEW-WRITER','claim','OLD-WRITER','--owner','b')
            out=io.StringIO()
            with patch.object(taskboard,'ROOT',frozen),contextlib.redirect_stdout(out):taskboard.main(['next'])
            self.assertFalse(json.loads(out.getvalue()).get('writer',False))
        finally:self.root=main
    def test_ledger_legacy_entry_without_flag_counts_as_writer(self):
        # CR4R-01: a running entry written before the writer flag existed is a writer even if this checkout's
        # plan calls the same id a non-writer (the claiming checkout's plan may have said writer=true).
        tasks={'M0-T01':{'task_id':'M0-T01','status':'done','owner':'seed','note':'TEST seed'},
               'M0-T04':{'task_id':'M0-T04','status':'running','owner':'old-tool','note':''}}
        (self.root/'.hoh-dev').mkdir(exist_ok=True)
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':tasks,'history':[]}))
        self.assertFalse(next(t for t in load_json(self.root/'contracts/development-tasks.json')['tasks'] if t['id']=='M0-T04')['writer'])
        self.refused_with('M0-T04','claim','M0-T02','--owner','dev')
    def test_ledger_unknown_running_task_counts_as_writer(self):
        (self.root/'.hoh-dev').mkdir(exist_ok=True)
        tasks={'M0-T01':{'task_id':'M0-T01','status':'done','owner':'seed','note':'TEST seed'},'FUTURE-T01':{'task_id':'FUTURE-T01','status':'running','owner':'x','note':''}}
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':tasks,'history':[]}))
        self.refused_with('FUTURE-T01','claim','M0-T02','--owner','dev')  # legacy entry without a writer flag fails closed
        self.assertEqual(self.ledger('claim','M0-T04','--owner','dev'),0)  # non-writer work is not blocked
    def test_ledger_next_respects_single_writer(self):
        self.seed(['M0-T01','M0-T02','M0-T03','M0-T04','M0-T05','M1-T01'])
        self.assertEqual(self.ledger('claim','M1-T02','--owner','a'),0)
        out=io.StringIO()
        with patch.object(taskboard,'ROOT',self.root),contextlib.redirect_stdout(out):taskboard.main(['next'])
        shown=json.loads(out.getvalue());self.assertFalse(shown.get('writer',False),shown);self.assertIn('writer task is running',shown.get('message',''))
    def test_ledger_shared_by_linked_worktrees(self):
        repo=self.root/'repo';repo.mkdir()
        for cmd in (['init','-q'],['config','user.email','t@invalid.example'],['config','user.name','t'],['commit','-q','--allow-empty','-m','x'],['worktree','add','-q',str(self.root/'soak')]):
            subprocess.run(['git','-C',str(repo),*cmd],check=True,capture_output=True)
        self.assertEqual(taskboard.ledger_root(self.root/'soak').resolve(),repo.resolve())
        self.assertEqual(taskboard.ledger_root(repo).resolve(),repo.resolve())
    def test_ledger_outline_requirements_not_ready(self):
        tasks=load_json(self.root/'contracts/development-tasks.json')['tasks']
        self.seed([t['id'] for t in tasks if t['milestone'] in ('M0','M1','M2','V01')]+['M3-T01'])
        req={r['id']:r for r in load_json(self.root/'contracts/requirements.json')['requirements']}
        target=next(t for t in tasks if t['milestone']=='M3' and t['kind']=='implement')
        if all(req[r]['spec_maturity']=='detailed' for r in target['requirements']):self.skipTest('M3 already detailed')
        self.refused('claim',target['id'],'--owner','dev')
    def test_ledger_reopen_blocks_started_dependents(self):
        self.seed(['M0-T01','M0-T02'])
        self.assertEqual(self.ledger('claim','M0-T03','--owner','dev'),0)
        self.assertEqual(self.ledger('set','M0-T01','pending','--note','preflight was wrong'),0)
        self.assertEqual(self.state('M0-T02')['status'],'blocked');self.assertEqual(self.state('M0-T03')['status'],'blocked')

class TestSshdExtraction(unittest.TestCase):
    # CR4R-03/CR5R-02/CR5R-03: extract (never install) the distribution sshd, accept packages without the
    # OpenSSH >= 9.8 helpers, and leave nothing behind when download or extraction fails.
    VERSION='1:9.6p1-3ubuntu13.20'
    def setUp(self):
        t=tempfile.TemporaryDirectory(prefix='hoh-sshd-');self.addCleanup(t.cleanup);self.tmp=Path(t.name)
        self.fake=self.tmp/'bin';self.fake.mkdir();self.tools=self.tmp/'tools';self.calls=self.tmp/'calls.log'
        (self.fake/'apt-get').write_text('#!/bin/sh\necho "apt-get $*" >> "$CALLS"\n'
            '[ "$1" = download ] || exit 64\n'
            'if [ "$MODE" = download-fail ]; then echo partial > partial.deb; exit 100; fi\n'
            'v="${2#openssh-server=}"; echo "Get:1 http://archive.example openssh-server $v [9 kB]"\n'
            'echo deb > "openssh-server_${v}_amd64.deb"; echo "Fetched 9 kB in 0s (9 kB/s)"\n')
        (self.fake/'dpkg-deb').write_text('#!/bin/sh\n[ "$1" = -x ] || exit 64\nd="$3"\n'
            'if [ "$MODE" = corrupt ]; then mkdir -p "$d/usr"; echo junk > "$d/usr/half"; exit 2; fi\n'
            'mkdir -p "$d/usr/sbin" "$d/usr/lib/openssh"\n'
            '[ "$MODE" = no-sshd ] || { printf "#!/bin/sh\\n" > "$d/usr/sbin/sshd"; chmod +x "$d/usr/sbin/sshd"; }\n'
            'if [ "$MODE" = openssh10 ]; then for b in sshd-session sshd-auth; do printf "#!/bin/sh\\n" > "$d/usr/lib/openssh/$b"; chmod +x "$d/usr/lib/openssh/$b"; done; fi\n')
        for f in self.fake.iterdir():f.chmod(0o755)
    def run_extract(self,mode):
        env=os.environ|{'PATH':str(self.fake)+os.pathsep+os.environ['PATH'],'MODE':mode,'CALLS':str(self.calls),'HOH_TEST_SSHD_VERSION':self.VERSION}
        return subprocess.run(['bash',str(ROOT/'scripts/dev/extract-test-sshd.sh'),str(self.tools)],capture_output=True,text=True,timeout=30,env=env)
    def leftovers(self):
        base=self.tools/'openssh-server'
        return sorted(p.name for p in base.iterdir() if p.name.startswith('.')) if base.is_dir() else []
    def dest(self):return self.tools/'openssh-server'/self.VERSION.replace(':','_')
    def test_package_without_helpers_is_usable(self):
        p=self.run_extract('openssh96');self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(p.stdout,str(self.dest())+'\n')  # CR6R-02: download logs never reach stdout
        self.assertIn('Fetched',p.stderr);self.assertTrue((self.dest()/'usr/sbin/sshd').is_file())
        self.assertFalse((self.dest()/'usr/lib/openssh/sshd-session').exists());self.assertEqual(self.leftovers(),[])
    def test_openssh10_layout_and_reuse(self):
        self.assertEqual(self.run_extract('openssh10').returncode,0)
        self.assertTrue((self.dest()/'usr/lib/openssh/sshd-auth').is_file())
        before=self.calls.read_text().count('apt-get download')
        p=self.run_extract('openssh10');self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(self.calls.read_text().count('apt-get download'),before)  # usable result is reused
    def test_failed_download_leaves_nothing(self):
        p=self.run_extract('download-fail');self.assertNotEqual(p.returncode,0)
        self.assertEqual(self.leftovers(),[]);self.assertFalse(self.dest().exists())
    def test_failed_extraction_leaves_nothing(self):
        p=self.run_extract('corrupt');self.assertNotEqual(p.returncode,0)
        self.assertEqual(self.leftovers(),[]);self.assertFalse(self.dest().exists())
    def test_package_without_sshd_is_rejected(self):
        p=self.run_extract('no-sshd');self.assertEqual(p.returncode,69);self.assertIn('lacks usr/sbin/sshd',p.stderr)
        self.assertEqual(self.leftovers(),[]);self.assertFalse(self.dest().exists())
    def test_stale_temp_dirs_removed_on_cache_hit(self):
        # CR6R-03: killed after publishing the result but before the EXIT trap; the next (cached) run cleans up.
        self.assertEqual(self.run_extract('openssh10').returncode,0)
        stale=self.tools/'openssh-server/.download.crash';stale.mkdir();(stale/'partial.deb').write_text('x')
        (self.tools/'openssh-server/.extract.crash').mkdir()
        p=self.run_extract('openssh10');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(p.stdout,str(self.dest())+'\n')
        self.assertEqual(self.leftovers(),[])
    def test_incomplete_earlier_result_is_replaced(self):
        (self.dest()/'usr').mkdir(parents=True);(self.tools/'openssh-server/.extract.stale').mkdir()
        p=self.run_extract('openssh96');self.assertEqual(p.returncode,0,p.stderr)
        self.assertTrue((self.dest()/'usr/sbin/sshd').is_file());self.assertEqual(self.leftovers(),[])
