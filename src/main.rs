use matching_engine::order::book::{tree_map, Book};
use matching_engine::order::{Order, Side};

fn main() {
    let mut book = tree_map::TreeMap::new();

    book.add(Order::new(1, "1".to_string(), Side::Buy, 1, 1)).expect("could not add order");
    book.add(Order::new(2, "2".to_string(), Side::Buy, 2, 1)).expect("could not add order");

    book.add(Order::new(3, "3".to_string(), Side::Sell, 1, 1)).expect("could not add order");
    book.add(Order::new(4, "4".to_string(), Side::Sell, 2, 1)).expect("could not add order");
    book.add(Order::new(5, "5".to_string(), Side::Sell, 2, 1)).expect("could not add order");

    println!("{:?}", book.depth(10));

    book.cancel(2).expect("could not cancel order");
    book.cancel(5).expect("could not cancel order");
    println!("{:?}", book.depth(10));

    book.match_orders();
    println!("{:?}", book.depth(10));
}
