#!/usr/bin/env python3
"""Write or check BUNDLE-MANIFEST.json for a kit release (Git-tracked and new unignored files).

Git history is the audit trail during development; the manifest only identifies
the exact file set of a distributed kit version. Regenerate it when KIT_VERSION changes.
"""
from __future__ import annotations
import argparse,json,subprocess,sys
from common import ROOT,load_json,sha256

NAME='BUNDLE-MANIFEST.json'

def build()->dict:
    out=subprocess.run(['git','ls-files','-z','--cached','--others','--exclude-standard'],cwd=ROOT,capture_output=True,check=True).stdout.decode()
    paths=sorted({p for p in out.split('\0') if p and p!=NAME and (ROOT/p).is_file()})
    lock=load_json(ROOT/'contracts/baseline-lock.json')
    return {'schema_version':'1.1','kit_version':(ROOT/'KIT_VERSION').read_text().strip(),'kind':'design_and_ai_development_kit',
            'product_implemented':False,'baseline':lock['baseline_id'],'baseline_revision':lock.get('revision',1),
            'entries':[{'path':p,'bytes':(ROOT/p).stat().st_size,'sha256':sha256(ROOT/p)} for p in paths]}

def main(argv=None)->int:
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--check',action='store_true');a=p.parse_args(argv)
    data=build();text=json.dumps(data,ensure_ascii=False,indent=2)+'\n'
    if a.check:
        current=(ROOT/NAME).read_text(encoding='utf-8') if (ROOT/NAME).exists() else ''
        if current!=text:print(f'{NAME} is stale; run python3 tools/bundle_manifest.py',file=sys.stderr);return 1
        print(f'{NAME} matches {len(data["entries"])} files.');return 0
    (ROOT/NAME).write_text(text,encoding='utf-8');print(f'wrote {NAME} ({len(data["entries"])} files)');return 0
if __name__=='__main__':sys.exit(main())
