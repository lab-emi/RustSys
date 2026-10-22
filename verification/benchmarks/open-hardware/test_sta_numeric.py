# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Constant-size public-report parsing controls; expected RSS below 64 MiB."""
import unittest

from sta_benchmark import completed_numeric_result


def receipt(period='6171.86',frequency='0.16'):
    return (f'time 1ns\ncapacitance 1pF\nBENCH_CLOCK_MIN_PERIOD_BEGIN\n'
            f'bench_clk period_min = {period} fmax = {frequency}\n'
            'BENCH_CLOCK_MIN_PERIOD_END\nBENCH_CONSTRAINT_CHECK_BEGIN\n'
            'BENCH_CONSTRAINT_CHECK_END\n')


class PublicReportRounding(unittest.TestCase):
    def test_actual_slow_full_core_receipt(self):
        r=completed_numeric_result(receipt(),' ',{'status':'ok'})
        self.assertAlmostEqual(r['fmax_mhz'],1000/6171.86)
        self.assertEqual(r['reported_fmax_mhz'],0.16)

    def test_ordinary_frequency_and_scientific_notation(self):
        for period,frequency in [('27.93','35.80'),('2.793e1','3.580e1')]:
            self.assertIsNotNone(completed_numeric_result(receipt(period,frequency),'',{'status':'ok'}))

    def test_rounding_to_zero_keeps_positive_period_frequency(self):
        r=completed_numeric_result(receipt('1000000.00','0.00'),'',{'status':'ok'})
        self.assertAlmostEqual(r['fmax_mhz'],0.001)

    def test_inconsistent_pair_is_rejected(self):
        self.assertIsNone(completed_numeric_result(receipt(frequency='0.17'),'',{'status':'ok'}))

    def test_incomplete_or_wrong_units_or_error_is_rejected(self):
        for output,errors,status in [
            (receipt(),'', 'timeout'),
            (receipt(),'BENCH_STA_ERROR: incomplete','ok'),
            (receipt().replace('time 1ns','time 1ps'),'','ok'),
            (receipt().replace('BENCH_CONSTRAINT_CHECK_END',''),'','ok'),
        ]:
            self.assertIsNone(completed_numeric_result(output,errors,{'status':status}))

    def test_invalid_or_extreme_numbers_are_bounded(self):
        for period,frequency in [('0','1'),('-1','1'),('1e999999999','1'),('NaN','1'),('1','-1')]:
            self.assertIsNone(completed_numeric_result(receipt(period,frequency),'',{'status':'ok'}))


if __name__=='__main__':unittest.main()
