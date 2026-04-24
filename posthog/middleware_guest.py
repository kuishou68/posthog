"""Single-gate guest deflection middleware.

All guest enforcement runs through this middleware. The request either matches a rule in
`GUEST_RULES` that explicitly allows it, or the request is deflected — `404` for API paths
(so guest clients can't enumerate the surface area) or `redirect("/guest")` for non-API
SPA routes (so the FE scene allowlist/landing page can take over).

Since `is_guest=True` now flips the AC layer's default from allow to deny, the per-resource
rules below are just a thin adapter: they pull `team_id` and `resource_id` out of the URL
(or the `X-PostHog-Scene-Resource` header for query endpoints) and ask
`UserAccessControl.access_level_for_object` whether the guest has any non-`none` level on
that specific object. All the AC-table + dashboard-tile cascade semantics live in the AC
layer itself (tile AC rows are written by `guest_grants.create_grant` at grant time).

Adding support for a new resource type is a single entry in `GUEST_RULES`.
"""

import re
from typing import cast

from django.http import HttpRequest, HttpResponse, JsonResponse
from django.shortcuts import redirect

from posthog.models import OrganizationMembership
from posthog.models.insight import Insight
from posthog.models.team.team import Team
from posthog.models.user import User
from posthog.rbac.user_access_control import NO_ACCESS_LEVEL, UserAccessControl

from products.dashboards.backend.models.dashboard import Dashboard
from products.notebooks.backend.models import Notebook

from ee.models.rbac.access_control import AccessControl


class GuestRule:
    """Base class for a guest deflection rule.

    Rules are tried in declaration order. The first rule whose `matches` returns a truthy
    match is the deciding rule: if its `allows` returns True the request is forwarded,
    otherwise the request is deflected. A request that matches no rule is deflected.
    """

    def __init__(self, pattern: str):
        self._pattern = re.compile(pattern)

    def matches(self, request: HttpRequest) -> re.Match | None:
        return self._pattern.match(request.path)

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        raise NotImplementedError


class AlwaysAllowed(GuestRule):
    """Identity, auth, static assets, the guest landing page, etc. — unconditionally allowed."""

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        return True


def _guest_membership_for_team(user: User, team_id: int) -> OrganizationMembership | None:
    """Fetch the guest membership tied to the org that owns this team, or `None` if the user
    isn't a guest there. Consolidates the `is_guest=True & correct org` lookup used by every
    rule below."""
    try:
        team = Team.objects.select_related("organization").get(id=team_id)
    except Team.DoesNotExist:
        return None
    return (
        OrganizationMembership.objects.filter(
            user=user,
            organization_id=team.organization_id,
            is_guest=True,
        )
        .select_related("organization")
        .first()
    )


def _has_any_team_ac_row(user: User, team_id: int) -> bool:
    """Does the guest have ANY AccessControl row scoped to this team? Used by the team-metadata
    rule — guests with zero grants have no business pulling team-wide metadata (themes, tags)."""
    membership = _guest_membership_for_team(user, team_id)
    if membership is None:
        return False
    return AccessControl.objects.filter(organization_member=membership, team_id=team_id).exists()


def _resolve_object(resource: str, resource_id: str, team_id: int):
    """Resolve a URL-style resource identifier to a concrete model instance we can pass to
    `UserAccessControl.access_level_for_object`. Returns `None` when the resource doesn't
    exist — the caller treats that as "not allowed" (guests can't enumerate)."""
    if resource == "dashboard":
        if not resource_id.isdigit():
            return None
        return Dashboard.objects.filter(id=int(resource_id), team_id=team_id).first()
    if resource == "insight":
        if resource_id.isdigit():
            return Insight.objects.filter(id=int(resource_id), team_id=team_id).first()
        return Insight.objects.filter(short_id=resource_id, team_id=team_id).first()
    if resource == "notebook":
        if resource_id.isdigit():
            return Notebook.objects.filter(id=int(resource_id), team_id=team_id).first()
        return Notebook.objects.filter(short_id=resource_id, team_id=team_id).first()
    return None


def _guest_has_access_to(user: User, team_id: int, resource: str, resource_id: str) -> bool:
    """Single decision point reused by every resource-bound rule.

    Returns True iff `UserAccessControl.access_level_for_object` resolves to a non-`none`
    level for the (guest, team, resource, resource_id) tuple. The dashboard-tile cascade is
    already handled inside the AC layer because `create_grant` writes tile AC rows at grant
    time; the middleware itself never needs to check parent dashboards.
    """
    membership = _guest_membership_for_team(user, team_id)
    if membership is None:
        return False
    try:
        team = Team.objects.get(id=team_id)
    except Team.DoesNotExist:
        return False

    obj = _resolve_object(resource, resource_id, team_id)
    if obj is None:
        return False

    uac = UserAccessControl(user=user, team=team)
    level = uac.access_level_for_object(obj, resource=resource)  # type: ignore[arg-type]
    return level is not None and level != NO_ACCESS_LEVEL


class TeamScopedMetadataRead(GuestRule):
    """GET-only team-scoped endpoints (themes, variables, tags, annotations, cohorts, quick filters).

    Insights and dashboards transitively depend on these to render. Allowed when the guest
    has any AC row on this team — otherwise they have no business reading team metadata.
    """

    def matches(self, request: HttpRequest) -> re.Match | None:
        if request.method != "GET":
            return None
        return super().matches(request)

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        team_id = int(match.group("team_id"))
        return _has_any_team_ac_row(user, team_id)


class GrantBoundResource(GuestRule):
    """`/api/.../(dashboards|insights|notebooks)/<id>` — allowed iff the AC layer grants this
    guest non-`none` access to the addressed object.
    """

    def __init__(self, resource: str, pattern: str):
        super().__init__(pattern)
        self._resource = resource

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        team_id = int(match.group("team_id"))
        resource_id = match.group("resource_id")
        return _guest_has_access_to(user, team_id, self._resource, resource_id)


class GrantBoundListFilter(GuestRule):
    """GET `/api/.../<resource>/?short_id=<id>` — FE scene loaders resolve by short_id. Allowed
    when the AC layer grants non-`none` access to the addressed object.
    """

    def __init__(self, resource: str, pattern: str, filter_key: str = "short_id"):
        super().__init__(pattern)
        self._resource = resource
        self._filter_key = filter_key

    def matches(self, request: HttpRequest) -> re.Match | None:
        if request.method != "GET":
            return None
        return super().matches(request)

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        filter_value = request.GET.get(self._filter_key)
        if not filter_value:
            return False
        team_id = int(match.group("team_id"))
        return _guest_has_access_to(user, team_id, self._resource, filter_value)


class SceneBoundQuery(GuestRule):
    """`POST|GET /api/.../query[/<kind>]/` — allowed iff the `X-PostHog-Scene-Resource` header
    identifies a resource the AC layer grants this guest. Header format: `resource:resource_id`.

    The query rescoper (PR #3) applies further team/object-level filters to the query body;
    this middleware rule only guards the endpoint reachability.
    """

    _SCENE_HEADER = "X-PostHog-Scene-Resource"
    _VALID_RESOURCES = ("dashboard", "insight", "notebook")

    def matches(self, request: HttpRequest) -> re.Match | None:
        if request.method not in {"POST", "GET"}:
            return None
        return super().matches(request)

    def allows(self, request: HttpRequest, user: User, match: re.Match) -> bool:
        header = request.headers.get(self._SCENE_HEADER)
        if not header or ":" not in header:
            return False
        resource, _, resource_id = header.partition(":")
        resource = resource.strip()
        resource_id = resource_id.strip()
        if not resource_id or resource not in self._VALID_RESOURCES:
            return False
        team_id = int(match.group("team_id"))
        return _guest_has_access_to(user, team_id, resource, resource_id)


_METADATA_ENDPOINTS = (
    "data_color_themes",
    "insight_variables",
    "quick_filters",
    "annotations",
    "cohorts",
    "tags",
)


def _metadata_pattern(endpoint: str) -> str:
    return rf"^/api/(?:environments|projects)/(?P<team_id>\d+)/{endpoint}/?$"


GUEST_RULES: list[GuestRule] = [
    AlwaysAllowed(r"^/api/users/@me(/.*)?$"),
    AlwaysAllowed(r"^/api/organizations/@current/?$"),
    AlwaysAllowed(r"^/api/projects/@current/?$"),
    AlwaysAllowed(r"^/api/environments/@current/?$"),
    AlwaysAllowed(r"^/login/?$"),
    AlwaysAllowed(r"^/logout/?$"),
    AlwaysAllowed(r"^/api/login/?$"),
    AlwaysAllowed(r"^/api/logout/?$"),
    AlwaysAllowed(r"^/reset(/.*)?$"),
    AlwaysAllowed(r"^/signup/verify_email(/.*)?$"),
    AlwaysAllowed(r"^/_preflight/?$"),
    AlwaysAllowed(r"^/static/.*$"),
    AlwaysAllowed(r"^/favicon\.ico$"),
    AlwaysAllowed(r"^/guest(/.*)?$"),
    *[TeamScopedMetadataRead(_metadata_pattern(endpoint)) for endpoint in _METADATA_ENDPOINTS],
    # Anchored with `$` so sub-actions (e.g. `/dashboards/4/sharing/`, `/dashboards/4/collaborators/`)
    # do NOT inherit the grant — they need their own rule if we ever want to expose them.
    # Viewers don't need sub-actions; without this anchor a dashboard grant would leak the sharing
    # config and collaborator list for the granted dashboard.
    GrantBoundResource(
        "dashboard",
        r"^/api/(?:environments|projects)/(?P<team_id>\d+)/dashboards/(?P<resource_id>\d+)/?$",
    ),
    GrantBoundResource(
        "insight",
        r"^/api/(?:environments|projects)/(?P<team_id>\d+)/insights/(?P<resource_id>[A-Za-z0-9]+)/?$",
    ),
    GrantBoundResource(
        "notebook",
        r"^/api/(?:environments|projects)/(?P<team_id>\d+)/notebooks/(?P<resource_id>[A-Za-z0-9-]+)/?$",
    ),
    GrantBoundListFilter(
        "insight",
        r"^/api/(?:environments|projects)/(?P<team_id>\d+)/insights/?$",
    ),
    GrantBoundListFilter(
        "notebook",
        r"^/api/(?:environments|projects)/(?P<team_id>\d+)/notebooks/?$",
    ),
    SceneBoundQuery(r"^/api/(?:environments|projects)/(?P<team_id>\d+)/query(/[A-Z][A-Za-z]*)?/?$"),
]


class GuestDeflectionMiddleware:
    """Deflects guest users from every endpoint not allowed by a rule in `GUEST_RULES`.

    Placement: after `AuthenticationMiddleware` (needs `request.user`) but before
    `ActiveOrganizationMiddleware` — same slot family as impersonation middlewares.
    """

    def __init__(self, get_response):
        self.get_response = get_response
        self._rules: list[GuestRule] = GUEST_RULES

    def __call__(self, request: HttpRequest) -> HttpResponse:
        if not self._user_is_guest(request):
            return self.get_response(request)

        user = cast(User, request.user)
        for rule in self._rules:
            match = rule.matches(request)
            if match and rule.allows(request, user, match):
                return self.get_response(request)

        return self._deflect(request)

    def _user_is_guest(self, request: HttpRequest) -> bool:
        user = getattr(request, "user", None)
        if user is None or not user.is_authenticated:
            return False
        return user.organization_memberships.filter(is_guest=True).exists()

    def _deflect(self, request: HttpRequest) -> HttpResponse:
        if request.path.startswith("/api/"):
            return JsonResponse({"detail": "Not found."}, status=404)
        # Defensive: /guest is covered by AlwaysAllowed above, but belt-and-suspenders —
        # returning 404 here prevents an infinite redirect loop if that rule is ever removed.
        if request.path.startswith("/guest"):
            return JsonResponse({"detail": "Not found."}, status=404)
        return redirect("/guest")
