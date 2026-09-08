"""Real background export lifecycle, progress, warnings and concurrency tests."""

import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

import insta360_rs as api
from export_fixtures import MEDIA_TOOLS_AVAILABLE, ExportFixtureMixin, cpu_config


@unittest.skipUnless(
    MEDIA_TOOLS_AVAILABLE, "ffmpeg and ffprobe fixture tools unavailable"
)
class ExportJobIntegrationTests(ExportFixtureMixin, unittest.TestCase):
    def start_frames(self, name="frames", **changes):
        options = dict(indices=[0, 2, 4], config=cpu_config())
        options.update(changes)
        return api.start_export_frames(self.source, self.root / name, **options)

    def poll_until_finished(self, job):
        deadline = time.monotonic() + 20
        snapshots = []
        while not job.is_finished():
            progress = job.progress()
            if progress is not None:
                snapshots.append(progress)
            self.assertLess(time.monotonic(), deadline, "native export did not finish")
            time.sleep(0.001)
        return snapshots

    def assert_progress(self, progress):
        self.assertIsInstance(progress, api.ExportProgress)
        self.assertIsInstance(progress.phase, api.ExportPhase)
        self.assertIsInstance(progress.completed, int)
        self.assertGreaterEqual(progress.completed, 0)
        self.assertGreaterEqual(progress.elapsed_seconds, 0)
        if progress.total is not None:
            self.assertIsInstance(progress.total, int)
            self.assertGreaterEqual(progress.total, progress.completed)
        if progress.media_time_seconds is not None:
            self.assertGreaterEqual(progress.media_time_seconds, 0)
        if progress.estimated_remaining_seconds is not None:
            self.assertGreaterEqual(progress.estimated_remaining_seconds, 0)
        with self.assertRaises(AttributeError):
            progress.completed = 0

    def test_completed_job_exposes_final_progress_and_backend(self):
        job = self.start_frames()
        self.assertIsInstance(job, api.ExportJob)
        self.assertIn("ExportJob(finished=", repr(job))
        snapshots = self.poll_until_finished(job)
        progress = job.progress()
        self.assert_progress(progress)
        self.assertEqual(progress.phase, api.ExportPhase.FINALIZING)
        self.assertEqual((progress.completed, progress.total), (3, 3))
        self.assertIsNone(progress.media_time_seconds)
        for snapshot in snapshots:
            self.assert_progress(snapshot)
        backend = job.backend()
        self.assertIsInstance(backend, api.BackendReport)
        self.assertEqual(backend.selected, api.EffectiveBackend.CPU)
        self.assertEqual(job.take_warnings(), [])
        result = job.wait()
        self.assert_cpu_result(result, 3)
        self.assertTrue(job.is_finished())
        self.assertEqual(job.progress().completed, progress.completed)
        self.assertEqual(job.backend().selected, result.backend.selected)

    def test_immediate_wait_preserves_events_generated_while_blocking(self):
        job = self.start_frames()
        result = job.wait()
        self.assert_cpu_result(result, 3)
        self.assert_progress(job.progress())
        self.assertEqual(job.progress().phase, api.ExportPhase.FINALIZING)
        self.assertEqual(job.progress().completed, 3)
        self.assertEqual(job.backend().selected, api.EffectiveBackend.CPU)
        self.assertEqual(job.take_warnings(), [])

    def test_completed_progress_is_authoritative_when_event_buffer_overflows(self):
        job = api.start_export_frames(
            self.source,
            self.root / "frames",
            timestamps=[index / 1000 for index in range(40)],
            config=cpu_config(),
        )
        deadline = time.monotonic() + 20
        # Deliberately leave progress/events unread until the worker finishes.
        # Each encoded file emits a progress event, exceeding the bounded queue.
        while not job.is_finished():
            self.assertLess(time.monotonic(), deadline, "native export did not finish")
            time.sleep(0.005)
        result = job.wait()
        self.assert_cpu_result(result, 40)
        progress = job.progress()
        self.assert_progress(progress)
        self.assertEqual(progress.phase, api.ExportPhase.FINALIZING)
        self.assertEqual((progress.completed, progress.total), (40, 40))
        self.assertIsNone(progress.media_time_seconds)
        self.assertEqual(job.backend().selected, result.backend.selected)

    def test_result_can_be_consumed_only_once(self):
        job = self.start_frames()
        job.wait()
        with self.assertRaisesRegex(RuntimeError, "already been consumed"):
            job.wait()
        self.assertIsNone(job.cancel())
        self.assertIsNone(job.cancel())
        self.assertTrue(job.is_finished())

    def test_worker_error_propagates_and_consumes_the_result(self):
        job = self.start_frames(indices=[100])
        with self.assertRaises(api.InvalidMediaError):
            job.wait()
        self.assertTrue(job.is_finished())
        with self.assertRaises(RuntimeError):
            job.wait()
        self.assertIsNone(job.cancel())
        self.assertEqual(list((self.root / "frames").iterdir()), [])

    def test_cancellation_is_idempotent_and_removes_partial_files(self):
        job = api.start_export_frames(
            self.source,
            self.root / "frames",
            timestamps=[index / 100000 for index in range(10000)],
            config=cpu_config(width=512, height=256),
        )
        self.assertIsNone(job.cancel())
        self.assertIsNone(job.cancel())
        with self.assertRaises(api.CancelledError):
            job.wait()
        self.assertTrue(job.is_finished())
        self.assertEqual([path for path in self.root.rglob("*") if path.is_file()], [])
        with self.assertRaises(RuntimeError):
            job.wait()

    def test_can_poll_and_cancel_from_another_thread_while_wait_blocks(self):
        job = api.start_export_frames(
            self.source,
            self.root / "frames",
            timestamps=[index / 100000 for index in range(10000)],
            config=cpu_config(width=512, height=256),
        )
        entered = threading.Event()

        def wait():
            entered.set()
            return job.wait()

        with ThreadPoolExecutor(max_workers=1) as executor:
            pending = executor.submit(wait)
            self.assertTrue(entered.wait(timeout=5))
            # Allow the waiter to enter its GIL-releasing native wait. Thousands
            # of distinct targets guarantee work remains pending at this point.
            time.sleep(0.02)
            try:
                self.assertFalse(pending.done())
                self.assertFalse(
                    job.is_finished(), "wait must not mark a running worker finished"
                )
                progress = job.progress()
                if progress is not None:
                    self.assert_progress(progress)
                backend = job.backend()
                if backend is not None:
                    self.assertEqual(backend.selected, api.EffectiveBackend.CPU)
                self.assertEqual(job.take_warnings(), [])
                with self.assertRaises(RuntimeError):
                    job.wait()
            finally:
                job.cancel()
            with self.assertRaises(api.CancelledError):
                pending.result(timeout=20)
        self.assertTrue(job.is_finished())
        self.assertEqual([path for path in self.root.rglob("*") if path.is_file()], [])

    def test_concurrent_waiters_allow_exactly_one_result_consumer(self):
        job = self.start_frames()
        barrier = threading.Barrier(4)

        def wait():
            barrier.wait(timeout=5)
            try:
                return job.wait()
            except RuntimeError as error:
                return error

        with ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(lambda _: wait(), range(4)))
        self.assertEqual(
            sum(isinstance(result, api.ExportResult) for result in results), 1
        )
        self.assertEqual(sum(isinstance(result, RuntimeError) for result in results), 3)

    def test_independent_jobs_can_run_concurrently_on_the_same_input(self):
        jobs = [
            self.start_frames(name=str(index), indices=[index]) for index in range(4)
        ]
        with ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(lambda job: job.wait(), jobs))
        for index, result in enumerate(results):
            self.assert_cpu_result(result, 1)
            self.assertEqual(result.outputs[0].name, f"frame_{index:08}.png")
        self.assertTrue(all(job.is_finished() for job in jobs))

    def test_async_start_takes_a_configuration_snapshot(self):
        config = cpu_config()
        job = self.start_frames(config=config)
        config.width = 0
        config.backend = api.ProcessingBackend.GPU
        result = job.wait()
        self.assert_cpu_result(result, 3)

    def test_auto_backend_reports_fallback_and_warnings_are_drained_once(self):
        job = self.start_frames(config=cpu_config(backend=api.ProcessingBackend.AUTO))
        result = job.wait()
        self.assertEqual(result.backend.requested, api.ProcessingBackend.AUTO)
        self.assertEqual(job.backend().selected, result.backend.selected)
        warnings = job.take_warnings()
        self.assertTrue(all(isinstance(warning, str) for warning in warnings))
        self.assertEqual(job.take_warnings(), [])
        if result.backend.selected == api.EffectiveBackend.CPU:
            failure = result.backend.fallback
            self.assertIsInstance(failure, api.GpuFailure)
            for field in ("code", "stage", "message"):
                self.assertIsInstance(getattr(failure, field), str)
                self.assertTrue(getattr(failure, field))
            if failure.adapter is not None:
                self.assertIsInstance(failure.adapter, api.GpuAdapterInfo)
            self.assertTrue(
                any("GPU" in warning and "CPU" in warning for warning in warnings)
            )
            with self.assertRaises(AttributeError):
                failure.message = "changed"
        else:
            self.assertEqual(result.backend.selected, api.EffectiveBackend.GPU)
            self.assertIsInstance(result.backend.adapter, api.GpuAdapterInfo)
            self.assertIsNone(result.backend.fallback)

    def test_forced_gpu_reports_unavailable_or_uses_gpu_without_cpu_fallback(self):
        job = self.start_frames(config=cpu_config(backend=api.ProcessingBackend.GPU))
        if not api.capabilities().gpu_available:
            with self.assertRaises(api.GpuUnavailableError):
                job.wait()
        else:
            result = job.wait()
            self.assertEqual(result.backend.requested, api.ProcessingBackend.GPU)
            self.assertEqual(result.backend.selected, api.EffectiveBackend.GPU)
            self.assertIsNone(result.backend.fallback)

    def test_async_software_video_export_and_progress(self):
        if not ({"libx265", "libkvazaar"} & set(api.capabilities().hevc_encoders)):
            self.skipTest("native FFmpeg lacks a software HEVC encoder")
        job = api.start_export_video(
            self.source,
            self.root / "video.mp4",
            config=cpu_config(),
            audio=api.AudioPolicy.DROP,
            acceleration=api.MediaAcceleration.SOFTWARE,
            start=0.2,
            duration=0.3,
        )
        self.assertIsInstance(job, api.ExportJob)
        result = job.wait()
        self.assert_cpu_result(result, 3)
        self.assert_progress(job.progress())
        self.assertEqual(job.progress().phase, api.ExportPhase.FINALIZING)
        self.assertEqual(job.progress().completed, 3)
        self.assertEqual(job.backend().selected, api.EffectiveBackend.CPU)


if __name__ == "__main__":
    unittest.main()
