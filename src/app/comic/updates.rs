use super::*;

const GLOBAL_CONCURRENCY: usize = 4;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(20);
const FINISHED_INTERVAL: u64 = 24 * 60 * 60;
const ERROR_INTERVAL: u64 = 60 * 60;

enum UpdateResult {
    Summaries(Vec<(String, domain::UpdateSummary)>),
    Detail(Box<Detail>),
}
pub(super) struct UpdateJob {
    group: String,
    books: Vec<Book>,
    job: Job<UpdateResult>,
}

fn group(book: &Book) -> String {
    let provider = if book.provider.is_empty() {
        "mangayun"
    } else {
        &book.provider
    };
    format!("{provider}\0{}", book.site)
}

fn take_batch(
    queue: &mut VecDeque<Book>,
    jobs: &[UpdateJob],
    after: &HashMap<String, Instant>,
    source: &dyn ComicSource,
    now: Instant,
) -> Option<(String, Vec<Book>, Duration)> {
    if jobs.len() >= GLOBAL_CONCURRENCY {
        return None;
    }
    let index = queue.iter().position(|book| {
        let key = group(book);
        after.get(&key).is_none_or(|at| now >= *at)
            && jobs.iter().filter(|job| job.group == key).count()
                < source.update_policy(book).concurrency.max(1)
    })?;
    let book = queue.remove(index)?;
    let key = group(&book);
    let policy = source.update_policy(&book);
    let mut books = vec![book];
    while books.len() < policy.batch_size.clamp(1, 50) {
        let Some(index) = queue.iter().position(|book| group(book) == key) else {
            break;
        };
        books.push(queue.remove(index)?);
    }
    Some((key, books, policy.interval))
}

impl State {
    pub(super) fn updates_running(&self) -> bool {
        !self.update_jobs.is_empty() || !self.update_queue.is_empty()
    }

    pub(in crate::app) fn refresh_on_home(
        &mut self,
        settings: &crate::settings::Settings,
        now: Instant,
    ) {
        if settings.comics.check_updates
            && self
                .last_update_started
                .is_none_or(|last| now.saturating_duration_since(last) >= UPDATE_INTERVAL)
        {
            self.queue_updates(now, false);
        }
    }

    pub(super) fn begin_updates(&mut self, now: Instant) {
        self.queue_updates(now, true);
    }

    fn queue_updates(&mut self, now: Instant, manual: bool) {
        if self.updates_running() {
            return;
        }
        let first = self.last_update_started.is_none();
        let timestamp = domain::now();
        let mut entries: Vec<_> = self
            .entries()
            .iter()
            .filter(|e| e.favorite)
            .filter(|e| {
                if manual || e.checked == 0 {
                    return true;
                }
                let interval = if e.check_error.is_some() {
                    ERROR_INTERVAL
                } else if matches!(e.book.status.as_str(), "已完结" | "完结" | "END") {
                    FINISHED_INTERVAL
                } else if first {
                    return true;
                } else {
                    UPDATE_INTERVAL.as_secs()
                };
                timestamp.saturating_sub(e.checked) >= interval
            })
            .collect();
        entries.sort_by_key(|e| {
            (
                matches!(e.book.status.as_str(), "已完结" | "完结" | "END"),
                std::cmp::Reverse(e.last_read),
                e.checked,
                e.book.key(),
            )
        });
        self.update_queue = entries.into_iter().map(|e| e.book.clone()).collect();
        self.update_total = self.update_queue.len();
        self.update_done = 0;
        self.last_update_started = Some(now);
        self.update_after.clear();
    }

    pub(super) fn poll_updates(&mut self, now: Instant) {
        let mut completed = Vec::new();
        self.update_jobs.retain(|job| {
            if let Some(result) = job.job.poll() {
                completed.push((job.books.clone(), result));
                false
            } else {
                true
            }
        });
        for (books, result) in completed {
            self.update_done += books.len();
            for book in books {
                let Some(mut entry) = self
                    .library
                    .as_ref()
                    .and_then(|lib| lib.get(&book.key()))
                    .filter(|entry| entry.favorite)
                    .cloned()
                else {
                    continue;
                };
                match &result {
                    Ok(UpdateResult::Summaries(summaries)) => {
                        if let Some((_, summary)) =
                            summaries.iter().find(|(key, _)| *key == book.key())
                        {
                            entry.refresh_summary(summary.clone());
                        } else {
                            entry.check_error = Some(
                                "来源未返回此漫画 / Comic missing from update response".into(),
                            );
                            entry.checked = domain::now();
                        }
                    }
                    Ok(UpdateResult::Detail(detail)) => entry.refresh((**detail).clone()),
                    Err(error) => {
                        entry.check_error = Some(error.to_string());
                        entry.checked = domain::now();
                    }
                }
                self.save(entry);
            }
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        if self.update_queue.is_empty() {
            return;
        }
        // Remove favorites cancelled while a previous request was in flight.
        let favorites: HashSet<_> = self
            .entries()
            .iter()
            .filter(|e| e.favorite)
            .map(|e| e.book.key())
            .collect();
        let before = self.update_queue.len();
        self.update_queue
            .retain(|book| favorites.contains(&book.key()));
        self.update_done += before - self.update_queue.len();
        while let Some((key, books, interval)) = take_batch(
            &mut self.update_queue,
            &self.update_jobs,
            &self.update_after,
            source.as_ref(),
            now,
        ) {
            let api = source.clone();
            let requested = books.clone();
            self.update_after.insert(key.clone(), now + interval);
            self.update_jobs.push(UpdateJob {
                group: key,
                books,
                job: Job::spawn(move |_| async move {
                    check(api.as_ref(), &requested, UPDATE_TIMEOUT).await
                }),
            });
        }
    }
}

async fn check(
    source: &dyn ComicSource,
    books: &[Book],
    timeout: Duration,
) -> anyhow::Result<UpdateResult> {
    tokio::time::timeout(timeout, async {
        if let Some(summaries) = source.update_summaries(books).await? {
            Ok(UpdateResult::Summaries(summaries))
        } else {
            anyhow::ensure!(books.len() == 1, "Unsupported update batch");
            Ok(UpdateResult::Detail(Box::new(
                source.detail(&books[0]).await?,
            )))
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("检查更新超时，已跳过 / Update check timed out; skipped"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{
        Site, UpdateSummary,
        source::{UpdatePolicy, UpdateSummaries},
    };
    use futures_util::future::BoxFuture;

    struct Source;
    impl ComicSource for Source {
        fn update_policy(&self, book: &Book) -> UpdatePolicy {
            UpdatePolicy {
                batch_size: if book.provider == "komiic" { 50 } else { 1 },
                ..Default::default()
            }
        }
        fn update_summaries<'a>(
            &'a self,
            books: &'a [Book],
        ) -> BoxFuture<'a, anyhow::Result<Option<UpdateSummaries>>> {
            Box::pin(async move {
                Ok((books[0].provider == "komiic").then(|| {
                    books
                        .iter()
                        .map(|b| (b.key(), UpdateSummary::default()))
                        .collect()
                }))
            })
        }
        fn detail<'a>(&'a self, book: &'a Book) -> BoxFuture<'a, anyhow::Result<Detail>> {
            Box::pin(async move {
                assert_ne!(
                    book.provider, "komiic",
                    "summary checks must not fetch directories"
                );
                if book.url == "slow" {
                    std::future::pending::<()>().await;
                }
                Ok(Detail {
                    book: book.clone(),
                    chapters: Vec::new(),
                    summary: None,
                })
            })
        }
        fn sites(&self) -> BoxFuture<'_, anyhow::Result<Vec<Site>>> {
            unreachable!()
        }
        fn search<'a>(&'a self, _: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<SearchGroup>>> {
            unreachable!()
        }
        fn pages<'a>(
            &'a self,
            _: &'a Book,
            _: &'a Chapter,
            _: bool,
        ) -> BoxFuture<'a, anyhow::Result<Vec<String>>> {
            unreachable!()
        }
        fn image_url(&self, _: &str, _: &str) -> anyhow::Result<reqwest::Url> {
            unreachable!()
        }
    }
    fn book(site: &str, id: usize) -> Book {
        Book {
            provider: "mangayun".into(),
            site: site.into(),
            url: id.to_string(),
            ..Default::default()
        }
    }
    fn pending(group: String, books: Vec<Book>) -> UpdateJob {
        UpdateJob {
            group,
            books,
            job: Job::spawn(|_| std::future::pending()),
        }
    }

    #[test]
    fn independent_sources_respect_spacing_and_global_and_per_source_limits() {
        let mut queue: VecDeque<_> = ["a", "a", "a", "b", "b", "b", "c"]
            .into_iter()
            .enumerate()
            .map(|(id, site)| book(site, id))
            .collect();
        let mut jobs = Vec::new();
        let mut after = HashMap::new();
        let start = Instant::now();
        for (now, expected) in [
            (start, "a"),
            (start, "b"),
            (start + Duration::from_millis(500), "a"),
            (start + Duration::from_millis(500), "b"),
        ] {
            let (key, batch, interval) =
                take_batch(&mut queue, &jobs, &after, &Source, now).unwrap();
            assert_eq!(batch[0].site, expected);
            assert_eq!(batch.len(), 1);
            after.insert(key.clone(), now + interval);
            jobs.push(pending(key, batch));
        }
        assert!(
            take_batch(
                &mut queue,
                &jobs,
                &after,
                &Source,
                start + Duration::from_secs(5)
            )
            .is_none()
        );
        jobs.remove(3); // Free one global slot; the two slow A requests still occupy their source.
        let (_, batch, _) = take_batch(
            &mut queue,
            &jobs,
            &after,
            &Source,
            start + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(batch[0].site, "b");
        assert_eq!(queue.iter().filter(|b| b.site == "a").count(), 1);
    }

    #[test]
    fn summary_batches_are_bounded_and_keep_other_sources_separate() {
        let mut queue: VecDeque<_> = (0..55)
            .map(|id| Book {
                provider: "komiic".into(),
                ..book("komiic", id)
            })
            .collect();
        queue.push_front(book("dm5", 100));
        let first = queue.pop_front().unwrap();
        let jobs = vec![pending(group(&first), vec![first])];
        let now = Instant::now();
        let after = HashMap::from([(group(&book("dm5", 1)), now + Duration::from_secs(1))]);
        let (_, batch, _) = take_batch(&mut queue, &jobs, &after, &Source, now).unwrap();
        assert_eq!(batch.len(), 50);
        assert!(batch.iter().all(|b| b.provider == "komiic"));
        assert_eq!(queue.len(), 5);
    }

    #[test]
    fn update_timeout_is_bounded_and_summary_checks_do_not_load_directories() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let books = [Book {
                provider: "komiic".into(),
                ..book("komiic", 1)
            }];
            assert!(matches!(
                check(&Source, &books, Duration::from_secs(1))
                    .await
                    .unwrap(),
                UpdateResult::Summaries(_)
            ));
            let slow = [Book {
                url: "slow".into(),
                ..book("dm5", 1)
            }];
            let result = check(&Source, &slow, Duration::from_millis(20)).await;
            assert!(result.err().unwrap().to_string().contains("timed out"));
            assert!(matches!(
                check(&Source, &[book("dm5", 2)], Duration::from_secs(1))
                    .await
                    .unwrap(),
                UpdateResult::Detail(_)
            ));
        });
    }

    #[test]
    fn completed_summaries_are_shown_while_another_request_is_still_pending() {
        let ctx = egui::Context::default();
        let mut app = visual_tests::settings_fixture(&ctx);
        let state = &mut app.comic;
        let books: Vec<_> = state
            .entries()
            .iter()
            .filter(|e| e.favorite)
            .take(3)
            .map(|e| e.book.clone())
            .collect();
        state
            .update_jobs
            .push(pending(group(&books[0]), vec![books[0].clone()]));
        let ready = books[1..].to_vec();
        let returned = vec![(
            books[1].key(),
            UpdateSummary {
                updated_at: "2026-11-05T00:00:00Z".into(),
                last_chapter: "999".into(),
                ..Default::default()
            },
        )];
        state.update_jobs.push(UpdateJob {
            group: "ready".into(),
            books: ready,
            job: Job::spawn(|_| async move { Ok(UpdateResult::Summaries(returned)) }),
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.update_done < 2 && Instant::now() < deadline {
            state.poll_updates(Instant::now());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.update_done, 2);
        assert_eq!(state.update_jobs.len(), 1);
        let entry = state
            .library
            .as_ref()
            .unwrap()
            .get(&books[1].key())
            .unwrap();
        assert!(entry.summary_unread);
        assert_eq!(entry.chapters.len(), 128);
        assert_eq!(entry.progress.as_ref().unwrap().page, 7);
        assert!(
            state
                .library
                .as_ref()
                .unwrap()
                .get(&books[2].key())
                .unwrap()
                .check_error
                .is_some(),
            "missing summaries must remain retryable"
        );
    }

    #[test]
    fn automatic_checks_deprioritize_finished_titles_and_back_off_failures_but_manual_checks_bypass_them()
     {
        let ctx = egui::Context::default();
        let mut app = visual_tests::settings_fixture(&ctx);
        let state = &mut app.comic;
        let library = state.library.as_mut().unwrap();
        library.entries.retain(|e| e.favorite);
        library.entries[0].book.status = "已完结".into();
        library.entries[0].checked = domain::now();
        library.entries[1].check_error = Some("unavailable".into());
        library.entries[1].checked = domain::now();
        let expected = library.entries.len() - 2;
        let now = Instant::now();
        state.queue_updates(now, false);
        assert_eq!(state.update_queue.len(), expected);
        state.update_queue.clear();
        state.begin_updates(now);
        assert_eq!(state.update_queue.len(), expected + 2);
        assert_eq!(state.update_queue.back().unwrap().status, "已完结");
    }
}
