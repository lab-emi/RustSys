# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Resume guards; external positive/negative checks exercise the actual solver."""
from pathlib import Path
import tempfile
import unittest

import resume_transition
import run
import transition


class ResumeGuards(unittest.TestCase):
    def test_incomplete_preparation_is_rejected(self):
        for method in ('unknown', transition.METHOD):
            with self.assertRaises(ValueError):
                resume_transition.check_preparation(Path('.'), {'method': method}, Path('missing'))

    def test_changed_source_is_rejected_before_reusing_models(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            source, abc = path / 'rtl.v', path / 'abc'
            source.write_text('original RTL')
            abc.write_text('test binary identity')
            proof = {'method': transition.METHOD,
                     'certificate': {key: True for key in resume_transition.PREPARATION_FLAGS},
                     'tools': {'abc': {'sha256': run.digest(abc)}},
                     'source_sha256': {str(source): run.digest(source)}}
            proof['certificate'].update(cec_partitioning=False, cec_matching='verified positional')
            source.write_text('changed RTL')
            with self.assertRaisesRegex(ValueError, 'changed'):
                resume_transition.check_preparation(path, proof, abc)

    def test_resume_copy_is_independent_and_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            source, destination = path / 'source', path / 'destination'
            source.mkdir()
            (source / 'result.json').write_text('old verdict')
            resume_transition.copy_preparation(source, destination)
            (destination / 'result.json').write_text('new verdict')
            self.assertEqual((source / 'result.json').read_text(), 'old verdict')
            (source / 'link').symlink_to(source / 'result.json')
            with self.assertRaisesRegex(ValueError, 'symlink'):
                resume_transition.copy_preparation(source, path / 'rejected')
