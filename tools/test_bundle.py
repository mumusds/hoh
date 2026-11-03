#!/usr/bin/env python3
"""Run development-kit tests and optionally write an auditable JSON summary."""
import argparse,json,platform,sys,time,unittest
from pathlib import Path
from common import ROOT,atomic_json,utcnow

def main():
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path);p.add_argument('--pattern',default='test_*.py');a=p.parse_args()
    start=utcnow();clock=time.monotonic()
    suite=unittest.defaultTestLoader.discover(str(ROOT/'tests/bundle'),pattern=a.pattern)
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    report={'schema_version':'1.0','scope':'BUNDLE_AND_MOCK_ONLY','started_at':start,'finished_at':utcnow(),'duration_seconds':time.monotonic()-clock,'tests_run':result.testsRun,'failures':[{'test':str(t),'traceback':e} for t,e in result.failures],'errors':[{'test':str(t),'traceback':e} for t,e in result.errors],'skipped':[{'test':str(t),'reason':e} for t,e in result.skipped],'status':'PASS' if result.wasSuccessful() else 'FAIL','python':platform.python_version(),'platform':platform.platform(),'product_accepted':False,'live_agent_invocations':0,'notes':'Synthetic gate fixtures are only generated under temporary test repositories; they are not product acceptance evidence.'}
    if a.output:atomic_json(a.output,report)
    return 0 if result.wasSuccessful() else 1
if __name__=='__main__':sys.exit(main())
