from posthog.test.base import BaseTest
from unittest.mock import MagicMock

from rest_framework.exceptions import ValidationError

from posthog.schema import AggregationType, RetentionFilter, RetentionQuery, TimeWindowMode

from posthog.hogql_queries.insights.retention.retention_validation_rules import DisallowCumulativeWith24HourWindows
from posthog.hogql_queries.validation.validation import QueryValidationContext


class TestRetentionValidationRules(BaseTest):
    def _context(self, query: RetentionQuery) -> QueryValidationContext:
        runner = MagicMock(query=query, team=self.team, user=None)
        return QueryValidationContext(query=query, team=self.team, user=None, runner=runner)

    def test_disallow_cumulative_with_24h_windows(self):
        query = RetentionQuery(
            retentionFilter=RetentionFilter(cumulative=True, timeWindowMode=TimeWindowMode.FIELD_24_HOUR_WINDOWS)
        )

        with self.assertRaises(ValidationError) as context:
            DisallowCumulativeWith24HourWindows().validate(self._context(query))

        self.assertIn("Cumulative retention is not supported for 24 hour windows.", str(context.exception))

    def test_retention_validation_happy_path(self):
        query = RetentionQuery(retentionFilter=RetentionFilter(totalIntervals=8, aggregationType=AggregationType.COUNT))

        DisallowCumulativeWith24HourWindows().validate(self._context(query))
