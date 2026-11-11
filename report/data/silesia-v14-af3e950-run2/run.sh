#!/bin/bash
cd ~/acz-eval/silesia-v14-af3e950-run2
ALL="dickens mozilla mr nci ooffice osdb reymont samba sao webster xml x-ray"
for l in "-4" "-3" "-2" "-1" "-0" "-4 --dedup"; do ./bench.sh "acz14 $l" ~/acz-bench/acz14b "$l" 3 $ALL >> rows.tsv 2>> err.log; done
date -u >> env.txt; echo DONE >> rows.tsv
