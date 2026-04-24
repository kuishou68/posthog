import { useValues } from 'kea'

import { LemonBanner, LemonSkeleton, LemonTable } from '@posthog/lemon-ui'

import { TZLabel } from 'lib/components/TZLabel'

import { dataDeletionLogic } from './dataDeletionLogic'

export function DataDeletionPreviewPanel(): JSX.Element {
    const { preview, previewLoading, previewScoped } = useValues(dataDeletionLogic)

    if (!previewScoped) {
        return (
            <LemonBanner type="info">
                Choose a start date and at least one scoping criterion (events, "delete all", or a HogQL predicate) to
                preview what would be deleted.
            </LemonBanner>
        )
    }

    if (previewLoading && !preview) {
        return <LemonSkeleton className="h-24" />
    }

    if (!preview) {
        return (
            <LemonBanner type="warning">
                Could not preview with the current criteria. Adjust the scope and try again.
            </LemonBanner>
        )
    }

    if (preview.count === 0) {
        return <LemonBanner type="info">Your current criteria match 0 events — nothing to delete.</LemonBanner>
    }

    return (
        <div className="flex flex-col gap-2">
            <LemonBanner type="warning">
                <b>{preview.count.toLocaleString()}</b> events would be affected
                {preview.min_timestamp && preview.max_timestamp && (
                    <>
                        {' '}
                        between <TZLabel time={preview.min_timestamp} /> and <TZLabel time={preview.max_timestamp} />
                    </>
                )}
                .
                {preview.truncated && (
                    <>
                        {' '}
                        The preview below is limited to {preview.limit.toLocaleString()} rows, but deletion will cover
                        every matching event.
                    </>
                )}
            </LemonBanner>
            <LemonTable
                dataSource={preview.rows}
                rowKey="uuid"
                columns={[
                    { title: 'Event', dataIndex: 'event', width: 180 },
                    {
                        title: 'Timestamp',
                        dataIndex: 'timestamp',
                        render: (value) => (value ? <TZLabel time={value as string} /> : '—'),
                    },
                    { title: 'Distinct ID', dataIndex: 'distinct_id', width: 200 },
                    {
                        title: 'Properties',
                        dataIndex: 'properties',
                        render: (value) => <code className="text-xs">{(value as string)?.slice(0, 200) ?? ''}</code>,
                    },
                ]}
                pagination={{ pageSize: 20 }}
            />
        </div>
    )
}
