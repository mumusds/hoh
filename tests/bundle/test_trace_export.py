"""REQ-032 / AC-032-01 (TEST-032-01, check traceability): the product's trace exports checked by their mirror in
scripts/product/traceability.py (docs/adr/ADR-039.md decision 3 and DC-T07b).

The traceability check runs the product's fault-variant export itself (crates/hoh-app/tests/trace.rs,
test_032_01_product_export_over_fault_variants_matches_the_mirror, through cargo on the candidate's tree): the fixture
project (2 REQs, 4 ACs, 3 Tasks, 1 candidate) whole and over three faults written into the product's store (one TEST's
evidence missing, a PASS on a STALE older candidate, the candidate row missing), four exports written into the check's
evidence directory with this run's nonce and the tree they were produced on. TEST-032-01's test here reads only those;
missing, unreadable or foreign exports fail it, and outside a traceability run it is skipped (never PASS).
tests/bundle/fixtures/trace-export.json is a snapshot regression only, never the scenario evidence."""
import importlib.util,json,os,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from helpers import ROOT,load_json,own_python


def load_mirror():
    spec=importlib.util.spec_from_file_location('traceability_mirror',ROOT/'scripts/product/traceability.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
tr=load_mirror()

SNAPSHOT=ROOT/'tests/bundle/fixtures/trace-export.json'
ORDER=('requirement','acceptance','task','candidate','test','evidence','result')
# The product's exports in the traceability check's evidence directory (docs/adr/ADR-039.md DC-T07b), by variant.
EXPORTS={'whole':'trace-export-whole.json','missing_evidence':'trace-export-missing-evidence.json',
         'stale_candidate':'trace-export-stale-candidate.json','missing_candidate':'trace-export-missing-candidate.json'}


def reach(graph,start):
    seen,todo={start},[start]
    while todo:
        for nxt in graph.get(todo.pop(),()):
            if nxt not in seen:seen.add(nxt);todo.append(nxt)
    return seen


class Exports(unittest.TestCase):
    """What the four exports of the fixture must show, item by item against the mirror."""
    def assert_chains(self,export,skip=()):
        """Every node but those of `skip` reaches every kind after its own along the chain and every kind before it
        against it (REQ-032 behaviour 1)."""
        forward=tr.links(export);backward={}
        for a,bs in forward.items():
            backward.setdefault(a,set())
            for b in bs:backward.setdefault(b,set()).add(a)
        for node in forward:
            if node in skip:continue
            at=ORDER.index(node[0])
            ahead={k for k,_ in reach(forward,node)};behind={k for k,_ in reach(backward,node)}
            self.assertTrue(set(ORDER[at:])<=ahead,f'{node}: forward reaches only {sorted(ahead)}')
            self.assertTrue(set(ORDER[:at+1])<=behind,f'{node}: backward reaches only {sorted(behind)}')
        return forward

    def assert_exports(self,exports):
        self.assertEqual(set(exports),set(EXPORTS))
        # Every product export, item by item: the mirror computes from its nodes what the product wrote.
        for name,e in exports.items():
            with self.subTest(variant=name):
                self.assertEqual(tr.chain(e),{k:e[k] for k in ('broken','orphan','stale')})
        e=exports['whole']
        self.assertEqual((len(e['requirements']),len(e['acceptance']),len(e['tasks']),len(e['candidates'])),(2,4,3,1))
        self.assertIsNotNone(e['candidate_id'])
        self.assertEqual({k:e[k] for k in ('broken','orphan','stale')},{'broken':[],'orphan':[],'stale':[]})
        forward=self.assert_chains(e)
        self.assertEqual({k for k,_ in forward},set(ORDER))
        # One TEST's evidence missing: only that TEST is broken, the other chains are whole both ways.
        bad=exports['missing_evidence']
        self.assertEqual(bad['broken'],[{'node':'test','id':'TEST-A-01','missing':'evidence'}])
        self.assertEqual((bad['orphan'],bad['stale']),([],[]))
        cut={('test','TEST-A-01'),('acceptance','AC-A-01'),('requirement','REQ-A')}
        self.assert_chains(bad,skip=cut)
        # A PASS on an older STALE candidate: an orphan, and stale.
        old=exports['stale_candidate']
        rebound=[{'ac_id':'AC-B-01','spec_revision':1,'candidate_id':'CAND-old','evidence_id':'evidence-AC-B-01',
                  'status':'PASS'}]
        self.assertEqual((old['broken'],old['orphan'],old['stale']),([],rebound,rebound))
        # The candidate row missing: its evidence and results are still there, each evidence names its missing link.
        gone=exports['missing_candidate']
        self.assertIsNone(gone['candidate_id'])
        self.assertEqual(len(gone['evidence']),4);self.assertEqual(len(gone['results']),4)
        self.assertEqual(gone['broken'],[{'node':'evidence','id':x['evidence_id'],'missing':'candidate'}
                                         for x in gone['evidence']])


class TraceExport(Exports):
    def fresh(self):
        """This traceability run's product exports (DC-T07b): each present, readable, of its variant, with this run's
        nonce and the candidate's tree."""
        where=os.environ.get('HOH_TRACE_EXPORTS')
        if where is None:
            self.skipTest('the product exports exist only in a traceability check run (docs/adr/ADR-039.md DC-T07b)')
        nonce,tree=os.environ.get('HOH_TRACE_NONCE'),os.environ.get('HOH_TRACE_TREE')
        self.assertTrue(nonce and tree,'the traceability run names no nonce or tree')
        out={}
        for variant,name in EXPORTS.items():
            path=Path(where)/name
            self.assertTrue(path.is_file() and not path.is_symlink(),f'{name}: no product export in this run')
            doc=json.loads(path.read_bytes())
            self.assertIsInstance(doc,dict,name)
            self.assertEqual((doc.get('variant'),doc.get('nonce'),doc.get('git_tree')),(variant,nonce,tree),
                             f'{name}: not this run\'s export of the candidate\'s tree')
            out[variant]=doc['export']
        return out

    def test_032_01_fresh_product_exports_agree_with_the_mirror(self):
        self.assert_exports(self.fresh())

    def test_the_committed_snapshot_agrees_with_the_mirror(self):
        """A snapshot regression of its own (DC-T07b (3)): never TEST-032-01's evidence."""
        self.assert_exports(load_json(SNAPSHOT))


def clean_env():
    return {k:v for k,v in os.environ.items() if not k.startswith('HOH_')}


# A stand-in for cargo (docs/adr/ADR-039.md DC-T07b (4)): `mode` says what it writes into HOH_TRACE_EXPORT_DIR. It
# records its argv and environment first.
STUB=r'''import json, os, subprocess, sys
from pathlib import Path
mode, snapshot, log = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
log.write_text(json.dumps({'argv': sys.argv[4:], 'dir': os.environ.get('HOH_TRACE_EXPORT_DIR'),
                           'nonce': os.environ.get('HOH_TRACE_EXPORT_NONCE')}))
if mode == 'disabled':
    sys.exit(0)
if mode == 'failing':
    sys.exit(101)
tree = subprocess.run(['git', 'rev-parse', 'HEAD^{tree}'], capture_output=True, text=True, check=True).stdout.strip()
nonce = os.environ['HOH_TRACE_EXPORT_NONCE']
if mode == 'another-tree':
    tree = '0' * 40
if mode == 'another-run':
    nonce = 'an-earlier-run'
names = json.loads(os.environ['STUB_EXPORTS'])
for variant, export in json.loads(snapshot.read_text()).items():
    doc = {'variant': variant, 'nonce': nonce, 'git_tree': tree, 'export': export}
    (Path(os.environ['HOH_TRACE_EXPORT_DIR']) / names[variant]).write_text(json.dumps(doc))
'''


class ProductStep(unittest.TestCase):
    """M3N-100 (docs/adr/ADR-039.md DC-T07b (4)): the real traceability.sh through tools/run_check.py on a candidate
    frozen in a fixture ROOT of its own, whose spec has TEST-032-01 on the traceability check and whose plan has the
    real plan's traceability entry; `cargo` on PATH is a stub. With the product step disabled, failing, or giving
    another tree's or another run's exports, TEST-032-01 is not PASS; with exports of this run and tree it is."""
    FILES=['tools/run_check.py','tools/candidate.py','tools/common.py','scripts/product/traceability.sh',
           'scripts/product/traceability.py','tests/bundle/helpers.py','tests/bundle/test_trace_export.py',
           'tests/bundle/fixtures/trace-export.json']+[f'contracts/{n}.schema.json' for n in
                                                     ('execution','test-result','live-session','soak-event','candidate','check-plan')]
    CARGO=['test','--locked','-p','hoh-app','--test','trace','--','--exact',
           'test_032_01_product_export_over_fault_variants_matches_the_mirror']

    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hoh-m3t07-product-step-');self.addCleanup(self.temp.cleanup)
        r=self.root=Path(self.temp.name)/'root';r.mkdir()
        self.bin=Path(self.temp.name)/'bin';self.bin.mkdir()
        for rel in self.FILES:(r/rel).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,r/rel)
        spec={'requirements':[{'id':'REQ-032','release_targets':['v1'],'release_class':'required',
                               'acceptance':[{'id':'AC-032-01','test_id':'TEST-032-01'}]}]}
        (r/'contracts/requirements.json').write_text(json.dumps(spec))
        (r/'contracts/acceptance-scenarios.json').write_text(
            json.dumps({'scenarios':[{'id':'TEST-032-01','check_id':'traceability'}]}))
        self.entry=load_json(ROOT/'tests/check-plan.json')['checks']['traceability']
        (r/'plan.json').write_text(json.dumps({'schema_version':'1.0','plan_id':'product-step-fixture',
                                               'checks':{'traceability':self.entry}}))
        (r/'.gitignore').write_text('.hoh-dev/\n__pycache__/\n')
        self.git('init','-q');self.git('add','.');self.git('commit','-qm','fixture')
        (r/'.hoh-dev').mkdir();(r/'.hoh-dev/environment.json').write_text('{"mode":"SYNTHETIC_PRODUCT_STEP_FIXTURE"}\n')
        self.path=own_python(self.bin,os.environ.get('PATH','/usr/bin:/bin'))
        (self.bin/'stub.py').write_text(STUB);self.runs=0

    def git(self,*args):
        subprocess.run(['git','-c','user.name=t','-c','user.email=t@invalid.example',*args],cwd=self.root,check=True,
                       env=clean_env()|{'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1'})

    def tool(self,*argv,env=None):
        return subprocess.run([sys.executable,*argv],cwd=self.root,capture_output=True,text=True,timeout=300,
                              env=clean_env()|{'PATH':self.path,'PYTHONDONTWRITEBYTECODE':'1'}|(env or {}))

    def traceability(self,mode):
        """The check run with `cargo` stubbed as `mode`: its exit, its TEST-032-01 statuses, its record, what the
        stub saw (None when it never ran)."""
        log=self.bin/f'cargo-{mode}.json'
        cargo=self.bin/'cargo'
        cargo.write_text(f'#!/bin/sh\nexec {sys.executable} {self.bin/"stub.py"} {mode} '
                         f'{self.root/"tests/bundle/fixtures/trace-export.json"} {log} "$@"\n');cargo.chmod(0o755)
        out=f'.hoh-dev/candidates/{mode}.json'
        r=self.tool('tools/candidate.py','freeze','--spec','contracts/requirements.json','--environment',
                    '.hoh-dev/environment.json','--test-plan','plan.json','--out',out)
        self.assertEqual(r.returncode,0,r.stderr)
        self.runs+=1;evidence=f'.hoh-dev/evidence/{self.runs}-traceability'
        r=self.tool('tools/run_check.py','--check-id','traceability','--candidate-file',out,'--out',evidence,
                    env={'STUB_EXPORTS':json.dumps(EXPORTS)})
        record=load_json(self.root/evidence/'execution.json')
        results=self.root/evidence/'artifacts/test-results.jsonl'
        lines=[json.loads(l) for l in results.read_text().splitlines() if l.strip()] if results.is_file() else []
        statuses=[l['status'] for l in lines if l['test_id']=='TEST-032-01']
        seen=load_json(log) if log.is_file() else None
        return r.returncode,statuses,record,seen,(self.root/evidence/'stderr.log').read_text()

    def test_the_check_plan_declares_the_exports(self):
        self.assertTrue(set(EXPORTS.values())<=set(self.entry['artifacts']),self.entry)

    def test_with_exports_of_this_run_and_tree_it_passes(self):
        rc,statuses,record,seen,err=self.traceability('faithful')
        self.assertIsNotNone(seen,'the check never ran the product step')
        self.assertEqual(seen['argv'],self.CARGO)
        self.assertEqual(Path(seen['dir']),self.root/record['artifacts'][0]['path'].rsplit('/',1)[0])
        self.assertEqual((rc,statuses,record['missing_artifacts']),(0,['PASS'],[]),err)

    def test_the_product_step_disabled_gives_no_pass(self):
        rc,statuses,record,seen,err=self.traceability('disabled')
        self.assertIsNotNone(seen,'the check never ran the product step')
        self.assertEqual(statuses,['FAIL'],err);self.assertNotEqual(rc,0)
        self.assertTrue(set(EXPORTS.values())<=set(record['missing_artifacts']),record)

    def test_a_failing_product_step_gives_no_pass(self):
        rc,statuses,_,seen,err=self.traceability('failing')
        self.assertIsNotNone(seen,'the check never ran the product step')
        self.assertEqual(statuses,['FAIL'],err);self.assertNotEqual(rc,0)

    def test_exports_of_another_tree_or_run_give_no_pass(self):
        for mode in ('another-tree','another-run'):
            with self.subTest(mode=mode):
                rc,statuses,record,seen,err=self.traceability(mode)
                self.assertIsNotNone(seen,'the check never ran the product step')
                self.assertEqual(statuses,['FAIL'],err);self.assertNotEqual(rc,0)
                self.assertEqual(record['missing_artifacts'],[])


if __name__=='__main__':unittest.main()
