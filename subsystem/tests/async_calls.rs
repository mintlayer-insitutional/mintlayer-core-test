// Copyright (c) 2022 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://spdx.org/licenses/MIT
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Author(s): L. Kuklinek

use subsystem::*;

mod helpers;

// Logger (as a subsystem)
pub struct Logger {
    prefix: String,
}

impl Logger {
    fn new(prefix: String) -> Self {
        Logger { prefix }
    }

    fn write(&self, message: &str) {
        logging::log::warn!("{}: {}", self.prefix, message);
    }
}

// Logging counter
pub struct Counter {
    count: u64,
    logger: Subsystem<Logger>,
}

impl Counter {
    fn new(logger: Subsystem<Logger>) -> Self {
        let count = 0u64;
        Self { count, logger }
    }

    async fn bump(&mut self) {
        self.count += 1;
        let message = format!("Bumped counter to {}", self.count);
        self.logger.call(move |logger| logger.write(&message)).await;
    }
}

#[test]
fn async_calls() {
    let runtime = helpers::init_test_runtime();
    common::concurrency::model(move || {
        runtime.block_on(async {
            let app = Manager::new("app");
            let logger = app.start_passive("logger", Logger::new("logging".to_string()));
            let counter = app.start_passive("counter", Counter::new(logger.clone()));

            app.start("test", |_call_rq: CallRequest<()>, _shut_rq| async move {
                logger.call(|l| l.write("starting")).await;
                // Bump the counter twice
                counter.call_async_mut(|c| Box::pin(c.bump())).await;
                counter.call_async_mut(|c| Box::pin(c.bump())).await;
                logger.call(|l| l.write("done")).await;
            });

            app.main().await
        })
    })
}
