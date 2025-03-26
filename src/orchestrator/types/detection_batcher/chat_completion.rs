/*
 Copyright FMS Guardrails Orchestrator Authors

 Licensed under the Apache License, Version 2.0 (the "License");
 you may not use this file except in compliance with the License.
 You may obtain a copy of the License at

     http://www.apache.org/licenses/LICENSE-2.0

 Unless required by applicable law or agreed to in writing, software
 distributed under the License is distributed on an "AS IS" BASIS,
 WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 See the License for the specific language governing permissions and
 limitations under the License.

*/
use std::collections::{btree_map, BTreeMap, HashMap, HashSet};

use tracing::{debug, warn};

use super::{Chunk, DetectionBatcher, Detections, DetectorId, InputId};
use crate::config::ChunkerType;

/// A batcher for chat completions.
pub struct ChatCompletionBatcher {
    detectors: Vec<DetectorId>,
    chunker_types: HashMap<DetectorId, ChunkerType>,
    state: BTreeMap<Chunk, (ChunkerType, Vec<Detections>)>,
    chunk_detector_count: HashMap<ChunkerType, usize>,
}

impl ChatCompletionBatcher {
    pub fn new(detectors: Vec<DetectorId>, chunker_types: HashMap<String, ChunkerType>) -> Self {
        let chunk_detector_count: HashMap<ChunkerType, usize> = chunker_types.iter().fold(
            HashMap::new(),
            |mut counts, (key, value)| {
                counts.entry(value.clone()).or_insert(0);
                *counts.entry(value.clone()).or_insert(0) += 1;
                counts
            },
        );

        println!("chunk detector count: {:?}", chunk_detector_count);

        Self {
            detectors,
            chunker_types,
            chunk_detector_count,
            state: BTreeMap::default(),
        }
    }
}

impl DetectionBatcher for ChatCompletionBatcher {
    // type Batch = (u32, Chunks, Detections); // placeholder, actual type TBD
    type Batch = (Chunk, Detections);

    fn push(
        &mut self,
        // NOTE: input_id maps to choice_index
        _input_id: InputId,
        detector_id: DetectorId,
        chunk: Chunk,
        detections: Detections,
    ) {

        if let Some(chunk_type) = self.chunker_types.get(&detector_id) {
            match self.state.entry(chunk) {
                btree_map::Entry::Vacant(entry) => {
                    // New chunk, insert entry
                    entry.insert((*chunk_type, vec![detections]));
                }
                btree_map::Entry::Occupied(mut entry) => {
                    // Existing chunk, push detections
                    entry.get_mut().1.push(detections);
                }
            }
        } else {
            // This should only happen if there is a bug when initializing this batcher
            warn!("Chunker type for {:?} not found", detector_id);
        }

    }

    fn pop_batch(&mut self) -> Option<Self::Batch> {
        // TODO: implement batching logic to align with requirements
        // ref: https://github.com/foundation-model-stack/fms-guardrails-orchestrator/blob/main/docs/architecture/adrs/005-chat-completion-support.md#streaming-response

        // Check if we have all detections for the next chunk
        if self
            .state
            .first_key_value()
            .is_some_and(|(_, (chunk_type, detections))| *chunk_type == ChunkerType::All && detections.len() == *self.chunk_detector_count.get(chunk_type).unwrap()) {

                debug!("processing All chunker type detectors");
                println!("reached in pop for All chunker");
                if self.state.len() == 1 {
                    println!("for All chunker met with condition state.len == 1");
                    // Only return detections from All type chunker, if all other types have been already sent out or don't exist
                    if let Some((chunk, (_, detections))) = self.state.pop_first() {
                        let mut detections: Detections = detections.into_iter().flatten().collect();
                        // Provide sorted detections within each chunk
                        detections.sort_by_key(|r| r.start);
                        return Some((chunk, detections));
                    }
                }
            // There are other elements, so do nothing
        }
        else if self
            .state
            .first_key_value()
            .is_some_and(|(_, (chunk_type, detections))| detections.len() == *self.chunk_detector_count.get(chunk_type).unwrap())
        {
            debug!("processing sentence chunker type detectors");
            println!("reached in pop for sentence chunker");
            // We have all detections for the chunk, remove and return it.
            if let Some((chunk, (_, detections))) = self.state.pop_first() {

                println!("reached in pop for sentence chunker internal condition");
                let mut detections: Detections = detections.into_iter().flatten().collect();
                // Provide sorted detections within each chunk
                detections.sort_by_key(|r| r.start);
                return Some((chunk, detections));
            }
        }
        None
    }
}


#[cfg(test)]
mod test {

    use std::task::Poll;

    use futures::StreamExt;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    use super::*;
    use crate::orchestrator::{
        types::{Detection, DetectionBatchStream},
        Error,
    };

    #[test]
    fn test_batcher_with_single_chunk_no_all_type() {
        let input_id = 0;
        let chunk = Chunk {
            input_start_index: 0,
            input_end_index: 0,
            start: 0,
            end: 24,
            text: "this is a dummy sentence".into(),
        };

        // Create a batcher that will process batches for 2 detectors
        let detectors = vec![DetectorId::from("hap"), DetectorId::from("pii")];
        let mut chunker_types: HashMap<String, ChunkerType> = HashMap::new();
        chunker_types.insert("hap".to_string(), ChunkerType::Sentence);
        chunker_types.insert("pii".to_string(), ChunkerType::Sentence);
        let mut batcher = ChatCompletionBatcher::new(detectors, chunker_types);

        // Push chunk detections for pii detector
        batcher.push(
            input_id,
            "pii".into(),
            chunk.clone(),
            vec![Detection {
                start: Some(5),
                end: Some(10),
                detector_id: Some("pii".into()),
                detection_type: "pii".into(),
                score: 0.4,
                ..Default::default()
            }]
            .into(),
        );

        // We only have detections for 1 detector
        // pop_batch() should return None
        assert!(batcher.pop_batch().is_none());

        // Push chunk detections for hap detector
        batcher.push(
            input_id,
            "hap".into(),
            chunk.clone(),
            vec![
                Detection {
                    start: Some(5),
                    end: Some(10),
                    detector_id: Some("hap".into()),
                    detection_type: "hap".into(),
                    score: 0.8,
                    ..Default::default()
                },
                Detection {
                    start: Some(15),
                    end: Some(20),
                    detector_id: Some("hap".into()),
                    detection_type: "hap".into(),
                    score: 0.8,
                    ..Default::default()
                },
            ]
            .into(),
        );

        // We have detections for 2 detectors
        // pop_batch() should return a batch containing 3 detections for the chunk
        let batch = batcher.pop_batch();
        assert!(
            batch.is_some_and(|(chunk, detections)| { chunk == chunk && detections.len() == 3 })
        );
    }


    #[test]
    fn test_batcher_with_single_chunk_all_type() {
        let input_id = 0;
        let chunk = Chunk {
            input_start_index: 0,
            input_end_index: 0,
            start: 0,
            end: 24,
            text: "this is a dummy sentence".into(),
        };

        // Create a batcher that will process batches for 2 detectors
        let detectors = vec![DetectorId::from("hap"), DetectorId::from("pii")];
        let mut chunker_types: HashMap<String, ChunkerType> = HashMap::new();
        chunker_types.insert("hap".to_string(), ChunkerType::All);
        chunker_types.insert("pii".to_string(), ChunkerType::All);
        let mut batcher = ChatCompletionBatcher::new(detectors, chunker_types);

        // Push chunk detections for pii detector
        batcher.push(
            input_id,
            "pii".into(),
            chunk.clone(),
            vec![Detection {
                start: Some(5),
                end: Some(10),
                detector_id: Some("pii".into()),
                detection_type: "pii".into(),
                score: 0.4,
                ..Default::default()
            }]
            .into(),
        );

        // We only have detections for 1 detector
        // pop_batch() should return None
        assert!(batcher.pop_batch().is_none());

        // Push chunk detections for hap detector
        batcher.push(
            input_id,
            "hap".into(),
            chunk.clone(),
            vec![
                Detection {
                    start: Some(5),
                    end: Some(10),
                    detector_id: Some("hap".into()),
                    detection_type: "hap".into(),
                    score: 0.8,
                    ..Default::default()
                },
                Detection {
                    start: Some(15),
                    end: Some(20),
                    detector_id: Some("hap".into()),
                    detection_type: "hap".into(),
                    score: 0.8,
                    ..Default::default()
                },
            ]
            .into(),
        );

        // We have detections for 2 detectors
        // pop_batch() should return a batch containing 3 detections for the chunk
        let batch = batcher.pop_batch();
        println!("{:?}", batch);
        assert!(
            batch.is_some_and(|(chunk, detections)| { chunk == chunk && detections.len() == 3 })
        );
    }

    #[test]
    fn test_batcher_with_out_of_order_chunks() {
        let input_id = 0;
        let chunks = [
            Chunk {
                input_start_index: 0,
                input_end_index: 56,
                start: 11,
                end: 56,
                text: " a powerful tool for the development \
                    of complex systems."
                    .into(),
            },
            Chunk {
                input_start_index: 56,
                input_end_index: 135,
                start: 20,
                end: 60,
                text: " a powerful tool for the development \
                    of complex systems."
                    .into(),
            },
            Chunk {
                input_start_index: 0,
                input_end_index: 135,
                start: 0,
                end: 135,
                text: " It has been used in many fields, such as \
                    computer vision and image processing."
                    .into(),
            },
        ];

        // Create a batcher that will process batches for 2 detectors
        let detectors = vec![DetectorId::from("hap"), DetectorId::from("pii")];
        let mut chunker_types: HashMap<String, ChunkerType> = HashMap::new();
        chunker_types.insert("hap".to_string(), ChunkerType::All);
        chunker_types.insert("pii".to_string(), ChunkerType::Sentence);
        let mut batcher = ChatCompletionBatcher::new(detectors, chunker_types);

        // Push All chunk detection for HAP detector
        batcher.push(
            input_id,
            "hap".into(),
            chunks[2].clone(),
            vec![Detection {
                start: Some(0),
                end: Some(135),
                detector_id: Some("hap".into()),
                detection_type: "hap".into(),
                score: 0.9,
                ..Default::default()
            }]
            .into(),
        );

        // Push chunk-1 detections for hap detector
        batcher.push(
            input_id,
            "pii".into(),
            chunks[1].clone(),
            Detections::default(), // no detections
        );

        // NOTE: if only All type gets pushed, then it will get poped currently
        // since batcher doesn't know how many chunks are supposed to be there
        // that are potentially incoming

        // We have not pushed pushed detection for Chunk-1 for pii
        assert!(batcher.pop_batch().is_none());

        // Push chunk-1 detections for hap detector
        batcher.push(
            input_id,
            "pii".into(),
            chunks[0].clone(),
            vec![Detection {
                start: Some(11),
                end: Some(20),
                detector_id: Some("pii".into()),
                detection_type: "pii".into(),
                score: 0.4,
                ..Default::default()
            }]
            .into(),
        );


        // We have all detections for chunk-1 and chunk-2
        // pop_batch() should return chunk-1 with 1 pii detection
        let batch = batcher.pop_batch();
        println!("batch 1: {:?}", batch);

        assert!(batch
            .is_some_and(|(chunk, detections)| { chunk == chunks[0] && detections.len() == 1 }));

        // pop_batch() should return chunk-2
        let batch = batcher.pop_batch();
        println!("batch 2: {:?}", batch);
        println!("batcher state: {:?}", batcher.state);
        assert!(batch
            .is_some_and(|(chunk, detections)| { chunk == chunks[1] && detections.len() == 1 }));

        // pop_batch() should return detection for All type chunk
        let batch = batcher.pop_batch();
        println!("batch 3: {:?}", batch);
        assert!(batch
            .is_some_and(|(chunk, detections)| { chunk == chunks[1] && detections.len() == 1 }));

        // batcher state should be empty as all batches have been returned
        assert!(batcher.state.is_empty());
    }

    // #[tokio::test]
    // async fn test_detection_batch_stream() -> Result<(), Error> {
    //     let input_id = 0;
    //     let chunks = [
    //         Chunk {
    //             input_start_index: 0,
    //             input_end_index: 10,
    //             start: 0,
    //             end: 56,
    //             text: " a powerful tool for the development \
    //                 of complex systems."
    //                 .into(),
    //         },
    //         Chunk {
    //             input_start_index: 11,
    //             input_end_index: 26,
    //             start: 56,
    //             end: 135,
    //             text: " It has been used in many fields, such as \
    //                 computer vision and image processing."
    //                 .into(),
    //         },
    //     ];

    //     // Create detection channels and streams
    //     let (pii_detections_tx, pii_detections_rx) =
    //         mpsc::channel::<Result<(InputId, DetectorId, Chunk, Detections), Error>>(4);
    //     let pii_detections_stream = ReceiverStream::new(pii_detections_rx).boxed();
    //     let (hap_detections_tx, hap_detections_rx) =
    //         mpsc::channel::<Result<(InputId, DetectorId, Chunk, Detections), Error>>(4);
    //     let hap_detections_stream = ReceiverStream::new(hap_detections_rx).boxed();

    //     // Create a batcher that will process batches for 2 detectors
    //     let n = 2;
    //     let batcher = ChatCompletionBatcher::new(n);

    //     // Create detection batch stream
    //     let streams = vec![pii_detections_stream, hap_detections_stream];
    //     let mut detection_batch_stream = DetectionBatchStream::new(batcher, streams);

    //     // Send chunk-2 detections for pii detector
    //     let _ = pii_detections_tx
    //         .send(Ok((
    //             input_id,
    //             "pii".into(),
    //             chunks[1].clone(),
    //             Detections::default(), // no detections
    //         )))
    //         .await;

    //     // Send chunk-1 detections for hap detector
    //     let _ = hap_detections_tx
    //         .send(Ok((
    //             input_id,
    //             "hap".into(),
    //             chunks[0].clone(),
    //             Detections::default(), // no detections
    //         )))
    //         .await;

    //     // Send chunk-2 detections for hap detector
    //     let _ = hap_detections_tx
    //         .send(Ok((
    //             input_id,
    //             "hap".into(),
    //             chunks[1].clone(),
    //             Detections::default(), // no detections
    //         )))
    //         .await;

    //     // We have all detections for chunk-2, but not chunk-1
    //     // detection_batch_stream.next() future should not be ready
    //     assert!(matches!(
    //         futures::poll!(detection_batch_stream.next()),
    //         Poll::Pending
    //     ));

    //     // Send chunk-1 detections for pii detector
    //     let _ = pii_detections_tx
    //         .send(Ok((
    //             input_id,
    //             "pii".into(),
    //             chunks[0].clone(),
    //             vec![Detection {
    //                 start: Some(10),
    //                 end: Some(20),
    //                 detector_id: Some("pii".into()),
    //                 detection_type: "pii".into(),
    //                 score: 0.4,
    //                 ..Default::default()
    //             }]
    //             .into(),
    //         )))
    //         .await;

    //     // We have all detections for chunk-1 and chunk-2
    //     // detection_batch_stream.next() should be ready and return chunk-1 with 1 pii detection
    //     let batch = detection_batch_stream.next().await;
    //     assert!(batch.is_some_and(|result| result
    //         .is_ok_and(|(chunk, detections)| chunk == chunks[0] && detections.len() == 1)));

    //     // detection_batch_stream.next() should be ready and return chunk-2 with no detections
    //     let batch = detection_batch_stream.next().await;
    //     assert!(batch.is_some_and(|result| result
    //         .is_ok_and(|(chunk, detections)| chunk == chunks[1] && detections.is_empty())));

    //     // detection_batch_stream.next() future should not be ready
    //     // as detection senders have not been closed
    //     assert!(matches!(
    //         futures::poll!(detection_batch_stream.next()),
    //         Poll::Pending
    //     ));

    //     // Drop detection senders
    //     drop(pii_detections_tx);
    //     drop(hap_detections_tx);

    //     // detection_batch_stream.next() should return None
    //     assert!(detection_batch_stream.next().await.is_none());

    //     Ok(())
    // }
}