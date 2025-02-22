use super::*;
use crate::relay::relay_none::*;

use crate::proto::lifetime::DEFAULT_LIFETIME;
use std::net::Ipv4Addr;
use std::str::FromStr;
use tokio::net::UdpSocket;
use util::{vnet::net::*, Error};

fn new_test_manager() -> Manager {
    let config = ManagerConfig {
        relay_addr_generator: Box::new(RelayAddressGeneratorNone {
            address: "0.0.0.0".to_owned(),
            net: Arc::new(Net::new(None)),
        }),
    };
    Manager::new(config)
}

fn random_five_tuple() -> FiveTuple {
    /* #nosec */
    FiveTuple {
        src_addr: SocketAddr::new(Ipv4Addr::new(0, 0, 0, 0).into(), rand::random()),
        dst_addr: SocketAddr::new(Ipv4Addr::new(0, 0, 0, 0).into(), rand::random()),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_packet_handler() -> Result<(), Error> {
    //env_logger::init();

    // turn server initialization
    let turn_socket = UdpSocket::bind("127.0.0.1:0").await?;

    // client listener initialization
    let client_listener = UdpSocket::bind("127.0.0.1:0").await?;
    let src_addr = client_listener.local_addr()?;
    let (data_ch_tx, mut data_ch_rx) = mpsc::channel(1);
    // client listener read data
    tokio::spawn(async move {
        let mut buffer = vec![0u8; RTP_MTU];
        loop {
            let n = match client_listener.recv_from(&mut buffer).await {
                Ok((n, _)) => n,
                Err(_) => break,
            };

            let _ = data_ch_tx.send(buffer[..n].to_vec()).await;
        }
    });

    let m = new_test_manager();
    let a = m
        .create_allocation(
            FiveTuple {
                src_addr,
                dst_addr: turn_socket.local_addr()?,
                ..Default::default()
            },
            Arc::new(turn_socket),
            0,
            DEFAULT_LIFETIME,
        )
        .await?;

    let peer_listener1 = UdpSocket::bind("127.0.0.1:0").await?;
    let peer_listener2 = UdpSocket::bind("127.0.0.1:0").await?;

    let channel_bind = ChannelBind::new(
        ChannelNumber(MIN_CHANNEL_NUMBER),
        peer_listener2.local_addr()?,
    );

    let port = {
        let a = a.lock().await;

        // add permission with peer1 address
        a.add_permission(Permission::new(peer_listener1.local_addr()?))
            .await;
        // add channel with min channel number and peer2 address
        a.add_channel_bind(channel_bind.clone(), DEFAULT_LIFETIME)
            .await?;

        a.relay_socket.local_addr().await?.port()
    };

    let relay_addr_with_host_str = format!("127.0.0.1:{}", port);
    let relay_addr_with_host = SocketAddr::from_str(&relay_addr_with_host_str)?;

    // test for permission and data message
    let target_text = "permission";
    let _ = peer_listener1
        .send_to(target_text.as_bytes(), relay_addr_with_host)
        .await?;
    let data = data_ch_rx
        .recv()
        .await
        .ok_or_else(|| Error::new("data ch closed".to_owned()))?;

    // resolve stun data message
    assert!(is_message(&data), "should be stun message");

    let mut msg = Message::new();
    msg.raw = data;
    msg.decode()?;

    let mut msg_data = Data::default();
    msg_data.get_from(&msg)?;
    assert_eq!(
        target_text.as_bytes(),
        &msg_data.0,
        "get message doesn't equal the target text"
    );

    // test for channel bind and channel data
    let target_text2 = "channel bind";
    let _ = peer_listener2
        .send_to(target_text2.as_bytes(), relay_addr_with_host)
        .await?;
    let data = data_ch_rx
        .recv()
        .await
        .ok_or_else(|| Error::new("data ch closed".to_owned()))?;

    // resolve channel data
    assert!(
        ChannelData::is_channel_data(&data),
        "should be channel data"
    );

    let mut channel_data = ChannelData {
        raw: data,
        ..Default::default()
    };
    channel_data.decode()?;
    assert_eq!(
        channel_bind.number, channel_data.number,
        "get channel data's number is invalid"
    );
    assert_eq!(
        target_text2.as_bytes(),
        &channel_data.data,
        "get data doesn't equal the target text."
    );

    // listeners close
    m.close().await?;

    Ok(())
}

#[tokio::test]
async fn test_create_allocation_duplicate_five_tuple() -> Result<(), Error> {
    //env_logger::init();

    // turn server initialization
    let turn_socket: Arc<dyn Conn + Send + Sync> = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let m = new_test_manager();

    let five_tuple = random_five_tuple();

    let _ = m
        .create_allocation(
            five_tuple.clone(),
            Arc::clone(&turn_socket),
            0,
            DEFAULT_LIFETIME,
        )
        .await?;

    let result = m
        .create_allocation(five_tuple, Arc::clone(&turn_socket), 0, DEFAULT_LIFETIME)
        .await;
    assert!(result.is_err(), "expected error, but got ok");

    Ok(())
}

#[tokio::test]
async fn test_delete_allocation() -> Result<(), Error> {
    //env_logger::init();

    // turn server initialization
    let turn_socket: Arc<dyn Conn + Send + Sync> = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let m = new_test_manager();

    let five_tuple = random_five_tuple();

    let _ = m
        .create_allocation(
            five_tuple.clone(),
            Arc::clone(&turn_socket),
            0,
            DEFAULT_LIFETIME,
        )
        .await?;

    assert!(
        m.get_allocation(&five_tuple).await.is_some(),
        "Failed to get allocation right after creation"
    );

    m.delete_allocation(&five_tuple).await;

    assert!(
        m.get_allocation(&five_tuple).await.is_none(),
        "Get allocation with {} should be nil after delete",
        five_tuple
    );

    Ok(())
}

#[tokio::test]
async fn test_allocation_timeout() -> Result<(), Error> {
    //env_logger::init();

    // turn server initialization
    let turn_socket: Arc<dyn Conn + Send + Sync> = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let m = new_test_manager();

    let mut allocations = vec![];
    let lifetime = Duration::from_millis(100);

    for _ in 0..5 {
        let five_tuple = random_five_tuple();

        let a = m
            .create_allocation(five_tuple, Arc::clone(&turn_socket), 0, lifetime)
            .await?;

        allocations.push(a);
    }

    tokio::time::sleep(lifetime + Duration::from_millis(100)).await;

    for allocation in allocations {
        let mut a = allocation.lock().await;
        assert!(
            a.close().await.is_err(),
            "Allocation should be closed if lifetime timeout"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_manager_close() -> Result<(), Error> {
    // env_logger::init();

    // turn server initialization
    let turn_socket: Arc<dyn Conn + Send + Sync> = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let m = new_test_manager();

    let mut allocations = vec![];

    let a1 = m
        .create_allocation(
            random_five_tuple(),
            Arc::clone(&turn_socket),
            0,
            Duration::from_millis(100),
        )
        .await?;
    allocations.push(a1);

    let a2 = m
        .create_allocation(
            random_five_tuple(),
            Arc::clone(&turn_socket),
            0,
            Duration::from_millis(200),
        )
        .await?;
    allocations.push(a2);

    tokio::time::sleep(Duration::from_millis(150)).await;

    log::trace!("Mgr is going to be closed...");

    m.close().await?;

    for allocation in allocations {
        let mut a = allocation.lock().await;
        assert!(
            a.close().await.is_err(),
            "Allocation should be closed if lifetime timeout"
        );
    }

    Ok(())
}
