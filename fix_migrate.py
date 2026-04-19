with open('src/bin/migrate.rs', 'r') as f:
    content = f.read()

content = content.replace('let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(empty_batch)].into_iter(), schema.clone()));', 'let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(empty_batch)].into_iter(), schema.clone())) as Box<dyn arrow::array::RecordBatchReader + Send>;')
content = content.replace('let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema.clone()));', 'let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema.clone())) as Box<dyn arrow::array::RecordBatchReader + Send>;')

with open('src/bin/migrate.rs', 'w') as f:
    f.write(content)
