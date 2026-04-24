import base64
import asyncio

import pytest
from posthog.test.base import BaseTest
from unittest.mock import MagicMock, patch

from django.core.cache import cache

from posthog.models.integration import GitHubIntegration, GitHubIntegrationError, Integration
from posthog.models.integration_repository_cache import GitHubRepositoryFullCache, IntegrationRepositoryCacheEntry


class TestGitHubRepositoryFullCache(BaseTest):
    def setUp(self):
        super().setUp()
        cache.clear()

    def _create_integration(self) -> Integration:
        return Integration.objects.create(
            team=self.team,
            kind="github",
            config={"installation_id": "INSTALL", "account": {"name": "PostHog"}},
            sensitive_config={"access_token": "ACCESS_TOKEN"},
        )

    def _readme_payload(self, text: str) -> dict:
        return {"content": base64.b64encode(text.encode("utf-8")).decode("ascii")}

    def _default_gh_api_responses(self, *, sha: str = "SHA1", readme: str = "# Hello", truncated: bool = False) -> dict:
        return {
            "/repos/PostHog/posthog": {
                "default_branch": "main",
                "description": "PostHog open-source",
                "topics": ["analytics", "observability"],
                "archived": False,
                "fork": False,
                "language": "Python",
            },
            "/repos/PostHog/posthog/branches/main": {"commit": {"sha": sha}},
            "/repos/PostHog/posthog/readme": self._readme_payload(readme),
            f"/repos/PostHog/posthog/git/trees/{sha}?recursive=1": {
                "tree": [
                    {"path": "README.md", "type": "blob"},
                    {"path": "src/app.py", "type": "blob"},
                    {"path": "src", "type": "tree"},  # non-blob is filtered out
                ],
                "truncated": truncated,
            },
        }

    def _patch_gh_api_get(self, responses: dict):
        def _side_effect(self_, path, *, timeout=10):
            if path not in responses:
                raise AssertionError(f"unexpected path {path}")
            return responses[path]

        return patch(
            "posthog.models.integration.GitHubIntegration._gh_api_get", autospec=True, side_effect=_side_effect
        )

    def _cache_for(self, integration: Integration) -> GitHubRepositoryFullCache:
        return GitHubRepositoryFullCache(GitHubIntegration(integration))

    def test_sync_full_cache_entry_populates_all_fields(self):
        integration = self._create_integration()
        responses = self._default_gh_api_responses()

        with self._patch_gh_api_get(responses):
            entry = self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

        assert entry.full_name == "PostHog/posthog"
        assert entry.team_id == self.team.id
        assert entry.description == "PostHog open-source"
        assert entry.topics == ["analytics", "observability"]
        assert entry.archived is False
        assert entry.fork is False
        assert entry.primary_language == "Python"
        assert entry.default_branch == "main"
        assert entry.default_branch_sha == "SHA1"
        assert entry.readme == "# Hello"
        assert entry.tree_paths == "README.md\nsrc/app.py"
        assert entry.tree_truncated is False

    def test_sync_full_cache_entry_skips_readme_refetch_when_sha_unchanged(self):
        integration = self._create_integration()
        IntegrationRepositoryCacheEntry.objects.create(
            integration=integration,
            team=self.team,
            full_name="PostHog/posthog",
            description="stale description",
            topics=["old"],
            archived=False,
            fork=False,
            primary_language="Ruby",
            default_branch="main",
            default_branch_sha="SHA1",
            readme="cached",
            tree_paths="old/path",
            tree_truncated=False,
        )

        responses = self._default_gh_api_responses(sha="SHA1")
        # Remove heavy endpoints to assert they are NOT called on the light path.
        responses.pop("/repos/PostHog/posthog/readme")
        responses.pop("/repos/PostHog/posthog/git/trees/SHA1?recursive=1")

        with self._patch_gh_api_get(responses):
            entry = self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

        assert entry.description == "PostHog open-source"  # metadata updated
        assert entry.primary_language == "Python"
        assert entry.readme == "cached"  # readme unchanged
        assert entry.tree_paths == "old/path"

    def test_sync_full_cache_entry_refetches_when_sha_changed(self):
        integration = self._create_integration()
        IntegrationRepositoryCacheEntry.objects.create(
            integration=integration,
            team=self.team,
            full_name="PostHog/posthog",
            default_branch="main",
            default_branch_sha="SHA1",
            readme="old readme",
            tree_paths="old/path",
        )
        responses = self._default_gh_api_responses(sha="SHA2", readme="# New README")

        with self._patch_gh_api_get(responses):
            entry = self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

        assert entry.default_branch_sha == "SHA2"
        assert entry.readme == "# New README"
        assert entry.tree_paths == "README.md\nsrc/app.py"

    def test_sync_full_cache_entry_handles_readme_fetch_failure_gracefully(self):
        integration = self._create_integration()
        responses = self._default_gh_api_responses()
        responses.pop("/repos/PostHog/posthog/readme")

        def _side_effect(self_, path, *, timeout=10):
            if path == "/repos/PostHog/posthog/readme":
                raise GitHubIntegrationError("readme missing", status_code=404)
            return responses[path]

        with patch(
            "posthog.models.integration.GitHubIntegration._gh_api_get",
            autospec=True,
            side_effect=_side_effect,
        ):
            entry = self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

        assert entry.readme == ""
        assert entry.tree_paths == "README.md\nsrc/app.py"

    def test_sync_full_cache_entry_handles_truncated_tree(self):
        integration = self._create_integration()
        responses = self._default_gh_api_responses(truncated=True)

        with self._patch_gh_api_get(responses):
            entry = self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

        assert entry.tree_truncated is True

    def test_sync_full_cache_entry_raises_on_missing_branch_sha(self):
        integration = self._create_integration()
        responses = self._default_gh_api_responses()
        responses["/repos/PostHog/posthog/branches/main"] = {"commit": {}}  # missing sha

        with self._patch_gh_api_get(responses), pytest.raises(GitHubIntegrationError, match="missing commit sha"):
            self._cache_for(integration).sync_full_cache_entry("PostHog/posthog")

    def test_sync_full_cache_entry_rejects_bare_name(self):
        integration = self._create_integration()

        with pytest.raises(ValueError, match="'owner/repo' format"):
            self._cache_for(integration).sync_full_cache_entry("posthog")

    def test_sync_full_cache_runs_with_bounded_concurrency(self):
        integration = self._create_integration()

        observed_in_flight: list[int] = []
        in_flight = 0
        lock = asyncio.Lock()

        async def fake_entry_async(full_name):
            nonlocal in_flight
            async with lock:
                in_flight += 1
                observed_in_flight.append(in_flight)
            await asyncio.sleep(0.01)
            async with lock:
                in_flight -= 1
            return MagicMock(full_name=full_name)

        repos = [{"full_name": f"PostHog/repo-{i}"} for i in range(20)]

        async def fake_list():
            return repos

        repo_cache = self._cache_for(integration)
        with (
            patch.object(repo_cache, "sync_full_cache_entry_async", side_effect=fake_entry_async),
            patch.object(repo_cache.github, "list_all_cached_repositories_async", side_effect=fake_list),
        ):
            results = asyncio.run(repo_cache.sync_full_cache(concurrency=3))

        assert len(results) == 20
        assert max(observed_in_flight) <= 3

    def test_sync_full_cache_returns_exceptions_inline(self):
        integration = self._create_integration()

        async def fake_entry_async(full_name):
            if full_name == "PostHog/broken":
                raise GitHubIntegrationError("boom")
            return MagicMock(full_name=full_name)

        async def fake_list():
            return [{"full_name": "PostHog/ok"}, {"full_name": "PostHog/broken"}]

        repo_cache = self._cache_for(integration)
        with (
            patch.object(repo_cache, "sync_full_cache_entry_async", side_effect=fake_entry_async),
            patch.object(repo_cache.github, "list_all_cached_repositories_async", side_effect=fake_list),
        ):
            results = asyncio.run(repo_cache.sync_full_cache())

        assert len(results) == 2
        broken = next(r for r in results if isinstance(r, GitHubIntegrationError))
        assert str(broken) == "boom"
