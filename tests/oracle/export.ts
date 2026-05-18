#!/usr/bin/env -S deno run --allow-read --allow-write
/**
 * Export language-neutral oracle cases from the TypeScript validator.
 *
 * This is intentionally not a TS-to-Rust transpiler. It calls selected
 * TypeScript reference functions and writes small JSON manifests that
 * Rust tests can consume. Add new exporters as more TS tests become
 * portable contracts.
 */

import { readEntities } from '../../vendor/bids-validator-2.4.1/src/schema/entities.ts'
import { parseGzip } from '../../vendor/bids-validator-2.4.1/src/files/gzip.ts'
import { parseTIFF } from '../../vendor/bids-validator-2.4.1/src/files/tiff.ts'
import { BIDSFileDeno } from '../../vendor/bids-validator-2.4.1/src/files/deno.ts'

type OracleManifest = {
  schemaVersion: 1
  generatedBy: string
  cases: unknown[]
}

const ROOT = repoRoot()
const CASES_DIR = new URL('./cases/', import.meta.url)

const ENTITY_INPUTS = [
  'task-rhymejudgment_bold.json',
  'sub-01',
  'dataset_description.json',
  'participants.tsv',
  'sub-01_ses-01_T1w.nii.gz',
  'sub-01_SEM.ome.zarr',
  'sub-01_task-nback_meg',
]

const GZIP_INPUTS = [
  'anon.gz',
  'stamped.gz',
  'commented.gz',
]

const TIFF_INPUTS = [
  {
    id: 'tiff:btif_id.ome.tif:ome',
    dataset: 'tests/data/ome-tiff',
    path: 'btif_id.ome.tif',
    parseOme: true,
  },
]

if (import.meta.main) {
  await Deno.mkdir(CASES_DIR, { recursive: true })
  await writeManifest('entities.json', entityCases())
  await writeManifest('gzip.json', await gzipCases())
  await writeManifest('tiff.json', await tiffCases())
}

function entityCases() {
  return ENTITY_INPUTS.map((filename) => ({
    id: `entities:${filename}`,
    kind: 'filename_parse',
    sourceTest: 'src/schema/entities.test.ts',
    input: filename,
    expected: readEntities(filename),
    status: 'required',
    capabilities: ['filename'],
  }))
}

async function gzipCases() {
  const dataset = 'tests/data/gzip'
  const datasetPath = pathFromRoot(dataset)
  const cases = []
  for (const path of GZIP_INPUTS) {
    const file = new BIDSFileDeno(datasetPath, path)
    cases.push({
      id: `gzip:${path}`,
      kind: 'gzip_parse',
      sourceTest: 'src/files/gzip.test.ts',
      dataset,
      path,
      maxBytes: 1024,
      expected: await parseGzip(file) ?? null,
      status: 'required',
      capabilities: ['gzip'],
    })
  }
  return cases
}

async function tiffCases() {
  const cases = []
  for (const input of TIFF_INPUTS) {
    const file = new BIDSFileDeno(pathFromRoot(input.dataset), input.path)
    cases.push({
      id: input.id,
      kind: 'tiff_parse',
      sourceTest: 'src/files/tiff.test.ts',
      dataset: input.dataset,
      path: input.path,
      parseOme: input.parseOme,
      expected: await parseTIFF(file, input.parseOme),
      status: 'required',
      capabilities: ['tiff', 'ome'],
    })
  }
  return cases
}

async function writeManifest(name: string, cases: unknown[]) {
  const manifest: OracleManifest = {
    schemaVersion: 1,
    generatedBy: 'tests/oracle/export.ts',
    cases,
  }
  const path = new URL(name, CASES_DIR)
  await Deno.writeTextFile(path, JSON.stringify(manifest, null, 2) + '\n')
  console.log(`wrote ${path.pathname}`)
}

function pathFromRoot(relative: string): string {
  return new URL(relative.replaceAll('\\', '/') + '/', ROOT).pathname
}

function repoRoot(): URL {
  return new URL('../../vendor/bids-validator-2.4.1/', import.meta.url)
}
